# Automated private VPS capacity measurements

`build/run-vps-capacity.py` prepares an exact source revision, builds its images,
runs a bounded capacity experiment and collects the evidence on the controller.
It uses the existing Ansible playbook, image-build service and
[`build/capacity.py`](../../build/capacity.py) measurement engine. Preparation and
image building are explicit options; each measurement creates a new run.

Use a dedicated private benchmark host prepared with the
[bootstrap controller](BOOTSTRAP.md), the pinned Ansible Python environment,
and a clean committed checkout. The full `--revision` must equal the controller
checkout's `HEAD`. The remote checkout must also be clean at that revision.
For later collection or recovery, use a clean checkout of the run's original
revision so the controller, host protocol and retained evidence agree.

The inventory must be a caller-owned, mode `0600` static YAML or JSON file
with exactly one `benchmark_hosts.hosts` entry, a private SSH key and trusted
host keys. `--limit` selects that exact inventory alias. Executable inventories,
plugins, YAML aliases/tags/duplicate keys, templates, group variables and
unsupported host fields are refused without invoking Ansible. The
[bootstrap inventory contract](BOOTSTRAP.md#rerun-and-advance-the-source-revision)
lists the accepted fields. Preparation uses one private validated JSON snapshot,
so later changes to the operator's inventory cannot redirect the operation.
The controller uses
direct SSH with strict host-key checking and noninteractive sudo; password-based
first access belongs to bootstrap. Private keys, inventory and generated
evidence remain outside tracked source. The controller refuses a public host,
running containers, active builds or tests, and unfinished cleanup records.

## Measure thirty active microphones per room

This example describes a six-vCPU test host with a quoted 2 Gbit/s port and
$12.32 monthly price. Replace the inventory and alias with your own private
benchmark host. Port speed and price are planning inputs, not measurements.

```sh
ops/ansible/.venv/bin/python build/run-vps-capacity.py \
  --inventory ops/ansible/inventory.local.test.yml --limit test_vps \
  --revision "$(git rev-parse HEAD)" \
  --prepare --build-images --label audio30-one-worker \
  --workload meetings --first-size 30 --steps 1 --meeting-size 30 \
  --speakers 30 --audio-only --chat-interval-ms 30000 \
  --server-cpus 1 --generator-cpus 4.8 --app-cpus 5 \
  --port-mbps 2000 --monthly-price 12.32 --runtime-seconds 900
```

The server has one CPU and one media worker; the generator has a separate
4.8-CPU quota. `--app-cpus 5` describes the intended deployment for projection,
while `--server-cpus` controls the measured server. These are separate budgets:
a projection to five app CPUs does not mean five CPUs were tested.

Every participant publishes continuous synthetic browser-profile audio and
receives all twenty-nine peers. The current speech profile uses 32 kbit/s RTP
payloads at fifty packets per second. Text uses the same signaling connections:
each participant sends a 128-byte public message approximately every thirty
seconds, staggered across the room. New text sends stop two seconds before the
measurement ends so acknowledgements and peer delivery can complete. This is a
specific speech and text workload; cameras and higher-bitrate music need their
own measurements.

Normal runs have a thirty-second warmup and a sixty-second measurement window,
in addition to their population-dependent join ramp. The server's participant
and broadcaster room limits both equal `--meeting-size` for meeting workloads.
A full thirty-person room therefore exercises the configured limit.

`--steps 1` tests only the chosen population. A pass establishes a lower bound;
it does not find the maximum. Repeat a population to observe variation, then
increase it in whole-room increments. For example, sixty participants with
`--meeting-size 30 --server-cpus 2 --generator-cpus 3.8` exercises two rooms and
two workers on the same six-vCPU host. A room's publishers stay on its primary
worker; receive-side routers can use other workers as the room grows. Additional
workers therefore need direct measurement rather than an assumption of linear
scaling within a room.

For an adaptive search, increase `--steps` and allow enough elapsed time for
all join ramps, warmups and measured intervals. The existing search rules and
resource limits remain authoritative; see [sizing a host](../../docs/performance.md#sizing-a-host).
Generator throttling invalidates a measurement and must not be reported as a
server limit. Repeat useful boundaries before making a hosting recommendation.

## Preparation, image identity and repeated runs

`--prepare` runs `site.yml --tags source,benchmark` with the exact revision and
both OS-maintenance flags set to `false`. It holds the canonical workload lock
through preparation. The controller supplies the selected SSH trust settings
as one-run overrides, preserving the stored inventory. Initial OS maintenance
belongs to bootstrap's explicit `--initial-maintenance` option.
The controller continuously checks the SSH lease while Ansible runs. Lease loss,
excess output or a deadline terminates and reaps the owned command group; an
interrupted preparation remains a failure that requires inspection before retry.

`--build-images` starts the canonical image-build service and observes its
result. Successful builds retain immutable local image identities in
`artifacts/<revision>/images.json`; the controller verifies both revision labels
and the nonroot runtime user before testing. The image-build service reuses an
already verified manifest. Failed builds retain their attempt logs on the host
under the same revision's artifact directory.

After a successful preparation/build, omit `--prepare --build-images` to repeat
measurements against those exact images. All other workload flags remain
explicit. No failed measurement is automatically restarted, and no earlier
evidence directory is overwritten. A new code revision needs its own prepared
checkout and verified images.

The measured server uses `--network none`; the generator shares its isolated
network namespace. There are no host port mappings, bridge, default route or
external network access. Bounded control requests run inside the server over
loopback using the image's Bash TCP support. Both containers use UID/GID
`10001:10001`, a read-only root, dropped capabilities, `no-new-privileges`, disabled
core dumps, CPU/PID/memory limits and an equal memory/swap ceiling. Docker logs
rotate at three 10 MiB files; Podman uses a size-bounded `k8s-file` log. The
generator writes to an anonymous `/results` volume, removed with its owning
container during normal cleanup and explicit recovery. This writable volume is
not a disk quota; keep the documented free-space reserve on the dedicated host.
These guest-room tests use
neither PostgreSQL nor a public TLS/TURN path. They measure synthetic forwarding
and signaling on the host, rather than browser speech decoding or Internet
delivery. In particular, `--port-mbps 2000` does not verify a provider's 2 Gbit/s
network claim. Shared-vCPU contention can also change between measurements.

## Bounds and verdicts

The remote workload runs in a named transient systemd service under the same
exclusive lock as the canonical benchmark and image builder. It is disabled
from restarting. `--runtime-seconds` bounds total worker time, including setup
and join ramps; the accepted range is 120–21,600 seconds, default 7,200. Systemd
allows a bounded cleanup interval after termination. A controller disconnect
does not remove that remote deadline.

Successful services retain their completed state until collection. The controller
recognizes `active/exited` with no main process as finished, records its result,
then stops that exact completed unit. Later collection uses the retained result
after systemd unloads the unit. An uncollected completed service still blocks a
new run, so collect its evidence before starting another experiment.

`--quick` shortens the measurement to thirty seconds. It cannot be combined
with a thirty-second chat interval, which also needs a two-second delivery
drain. Invalid combinations are rejected before a workload starts. Other input
bounds, the per-room text rate limit, and retained-evidence limits are enforced
by the controller and generator.

The controller returns zero only when every requested step is valid and passes,
the worker exits successfully, and final container inspection confirms cleanup.
Missing reports, invalid generator observations, workload failures, timeouts and
uncertain cleanup return nonzero. An adaptive search may retain useful passing
lower bounds before a failing step; its nonzero result preserves that distinction.

Subprocess stdout/stderr are drained concurrently and limited before bytes reach
disk or a memory buffer. General controller commands have 2 MiB per-stream limits;
remote JSON responses and worker logs have 8 MiB limits per stream. Archive
downloads have a separate 512 MiB ceiling. A limit failure stops the owned process
group, retains bounded evidence and cannot become a passing measurement. Docker
log reads request only the last 1,000 lines and retain at most 200,000 characters.

Audio coverage requires at least 95% of the expected packet count for every
continuous audio consumer, alongside the existing connection, sustained-media,
CPU, memory and datagram-drop checks. Text requires matching acknowledgements
and exactly one correct delivery to every intended peer within two seconds.
Inspect the observed ratios and latency distributions; these minimum coverage
gates alone are not a claim of decoded audio quality. The
[load-generator guide](../../load_tests/README.md) defines the detailed checks.

## Collected evidence

Each invocation creates `results/vps-capacity.<UTC-time>.<random>/` locally.
Its private `report.json` records the selected revision, run ID, original
workload settings, lifecycle status and verdict. Preparation logs, SSH operation
responses and failures remain alongside it. Successful collection also retains
`remote.tar.gz`, its SHA256 identity and an extracted `remote/` directory.
Archives accept only bounded regular files with safe relative paths; links,
special files, duplicate destinations and oversized contents are rejected.

Remote evidence lives under
`/srv/simplestchat-bench/results/capacity-controller.<run-id>/`. It includes:

- The request, command arguments, image identities and service invocation.
- `workload/calibration.json`, per-step reports, load-generator results and logs.
- The container ownership journal and before/after container identities.
- Worker outcome, final systemd result, bounded service logs and collection status.
- A separate recovery receipt if recovery was explicitly requested.

Within each workload step, untrusted generator JSON lives in `generator/`.
Host-generated `generator.log`, `server.log`, `step.json`, metrics and
`collection.json` remain outside it. Collection streams an uncompressed results
archive with a 144 MiB transfer ceiling, validates the entire member list before
extraction, and opens only new regular files with `O_EXCL` and `O_NOFOLLOW`.
Only `load_test_summary.json` (8 MiB) and `load_test_results.json` (128 MiB) may
complete a measurement. A timeout marker, missing file, failed transfer,
duplicate name, link, special file, traversal or oversized file invalidates it.
Physical tar headers and extended metadata are bounded before general parsing.
The host-generated collection manifest records the exact size and SHA-256 of
each accepted file and the transferred archive. After successful publication,
the redundant raw archive is removed; failed transfers retain bounded evidence.
JSON reads independently refuse links and nonregular files. Download expansion
counts all decompressed bytes, including metadata and padding, toward its 1 GiB
limit, and rejects trailing or concatenated compressed streams.

Diagnostic container inspections retain only identity and lifecycle fields.
They exclude container environment variables and scoped metrics credentials.
An interrupted run may lack a calibration or generator summary; its lifecycle
evidence still explains why it cannot count as a passing measurement.

## Collect or recover an interrupted run

Use the run ID from the controller's local `report.json`, with the same target
and original source revision. Collection waits for the existing unit to finish,
then retrieves its results without starting another measurement:

```sh
ops/ansible/.venv/bin/python build/run-vps-capacity.py \
  --inventory ops/ansible/inventory.local.test.yml --limit test_vps \
  --revision ORIGINAL_FULL_COMMIT --collect ORIGINAL_32_CHARACTER_RUN_ID
```

When the unit has stopped but verified owned containers remain, use `--recover`
instead of `--collect`. Recovery verifies each exact container ID, run label,
expected name and requested image again before stopping and removing it.
Generators are removed before servers because they share the server's network
namespace. Unknown or mismatched resources are preserved, and an uncertain
ownership record continues to block another run.

Recovery does not restart the workload or convert a failed measurement into a
pass. Its receipt separately records cleanup; the original failure remains
available. Both commands create new local evidence directories. Do not combine
them with `--prepare` or `--build-images`, and do not delete journals or locks to
bypass a refusal. Host failure and reboot can erase `/run` state; retain the
durable evidence and inspect ownership before performing another experiment.

## Verify container integration locally

The ordinary helper suite tests invalid archive metadata, output overflow,
inventory mutation and lease loss with isolated fixtures. An additional opt-in
check starts a real server and a tiny result-writing container using reviewed,
already available immutable images. It verifies isolated HTTP, read-only roots,
archive collection and anonymous-volume cleanup; it starts no media load and
does not claim capacity or image provenance:

```sh
CAPACITY_CONTAINER_TEST=1 CAPACITY_ENGINE=docker \
CAPACITY_SERVER_IMAGE=sha256:REPLACE_WITH_SERVER_IMAGE_ID \
CAPACITY_GENERATOR_IMAGE=sha256:REPLACE_WITH_GENERATOR_IMAGE_ID \
ops/ansible/.venv/bin/python -m unittest discover \
  -s ops/ansible/tests -p 'test_capacity_container.py'
```

Use `CAPACITY_ENGINE=podman` for an existing local Podman VM. The check never
pulls images or changes unrelated containers, networks or volumes.
