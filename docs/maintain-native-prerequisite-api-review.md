# Native fixed-launcher prerequisite API review

This proof route is one part of the complete maintenance runtime fixture. It does
not replace `release_public.py`, install a new default caller, change maintenance
policy or run the production workflow. The operational Python remains the reference.
The complete guest, real database/Caddy fixture, source witnesses, tests and actual
signed evidence remain required for the runtime claim.

The optional `maintain-guest` build now supplies the explicit
`release maintain-prerequisite guest` role selected by the Rust outer controller.
It admits exactly 52 project source pins and 15 operational helper pins, owns one
Ready/GO exchange and invokes the fixed run/init body once. The four former
proof-only Python guest/material sources are excluded from that request;
operational Python, Ops33 and Ansible remain unchanged. Default Ops/xtask
bootstrap builds keep native crypto disabled. The distributed feature requires
the authenticated patched-musl/OpenSSL 4.1.0 static kit and final static ELF proof;
local host OpenSSL availability does not establish that producer closure.

## Guest API and dependency decisions

The affected `C-FEATURE` boundary is additive and disabled by default. The hosted
recipe selects only `maintain-guest`; observed Cargo metadata must resolve the
three exact crypto dependencies and ASN.1 bigint support. `C-VALIDATE` and
`C-GOOD-ERR` apply to the closed request, render vocabulary, certificate policy
and bounded public failure categories. Runtime JSON is decoded before use;
private configuration, native errors and key bytes are never public diagnostics.
`C-SERDE` applies to data receipts, not live descriptor or process owners.

Guest render failures retain the fixed operation checkpoint: `render` for pure
source rendering, then `render-inert-handoff`, `render-files`, `render-helpers`,
`render-directories`, `render-assets`, `render-assets-receipt` or `render-tools`.
The producer assigns the next label before its existing operation; the guest
transaction saves the failed label before finalization. The paired outer decoder
admits only these literals, keeps the same fourteen-key guest receipt, and still
requires `complete` for success. This affected `C-VALIDATE`, `C-GOOD-ERR` and
`C-SERDE` review adds no raw path, filename, configuration, child output or errno.
It does not identify a failed branch within one group, alter an operation or
weaken finalization. The current signed four-case failure at the older broad
`render` checkpoint remains unexplained by these source diagnostics.

The safe OpenSSL adapter owns its contexts and TLS stream through the Rust
binding; certificate/key borrows remain within their owning objects. It adds no
first-party raw pointers, unsafe code or foreign callbacks. The pinned native
library supplies the ABI and cryptographic operations; X.509 parsing alone does
not establish signature or TLS validity. TLS requires explicit root trust,
hostname/SNI, the actual verified chain and bounded HTTP framing. First-party
panic/unwind behavior across foreign boundaries is not introduced or promised.

`C-DTOR-FAIL` and `C-DTOR-BLOCK` apply to acquired files, sockets, processes and
build workspaces. Their explicit finalizers preserve the first failure and
attempt all registered peers; dropping a handle is not positive cleanup proof.
Known raw-zero process effects are registered before later logging, close or
deadline refusals. Generated native build outputs have a separate owned tree;
the original source records remain checked, while the consumer kit retains its
complete immutable inventory. This is an affected-change review, not a
retrospective certification of the crate or workspace.

## Fixed entry and clock

In a prepared disposable Debian guest, the explicit guest role invokes exactly:

```text
/usr/local/libexec/simplestchat-public/simplestchat-ops \
  release maintain-prerequisite run --trial normal \
  --timeout-seconds 1050 --finalization-seconds 60
```

The only other trials are `cancel-owned-launcher` and
`timeout-stopped-launcher`. A dirty-source guest uses `normal`; only the actual
unchanged launcher can establish its Git refusal. There is no command, executable,
root, socket, host or duration selector. The internal `init` role requires actual
Linux PID1 and the original private paired socket on borrowed standard input.

The guest retains its original 1800-second interval, with 120 seconds reserved
and body ending at 1680. Native work receives one 1050-second interval with
60 seconds reserved. Its 990-second body contains 925 seconds of known debt:
launcher 900, READY/GO/result five each, TERM five and reap five. The remaining
65 seconds are an unmeasured local margin. Strict setup is at most 510 seconds;
the keeper checks 1170 seconds remain immediately before invocation. Cold setup
may fail this profile. These numbers are admissions, not completion guarantees.
The CLI does not receive the keeper's absolute deadline. The keeper enforces
its original process/output ceiling at the earlier of invocation plus 1050 and
its own body minus 120. Native run/init share one internal interval; a late keeper
observation or backstop makes the whole proof negative.

Run samples its original kernel monotonic origin before argument effects. Init
receives that exact absolute origin/body/end. Its owner is conservatively
backdated to that interval, so spawn and GO never grant another 1050 seconds.
All GO bounds must equal the original closed interval; extending or changing it
refuses. The full 925-second debt is checked again after GO. Result carries the
same fixed absolute result-phase ceiling from settlement, which the receiver
checks after receipt too. No role creates another whole budget.

## Descriptor and kernel identity

Run owns the exact fixed unshare process and canonical authenticated Ops original
at `/usr/local/libexec/simplestchat-public/simplestchat-ops` (root0755, one link).
No installed alias or bootstrap change is required. It executes
those original ELF files through parent proc-FD aliases; those aliases are only
valid in the checked outer namespace role. Init executes the unchanged fixed
`/usr/local/bin/simplestchat-public-deploy` path with no arguments, preserving its
`$0` behavior. Fixed launcher, timeout and original tool hashes/metadata remain held.
Each cleared native command environment sets `PYTHONDONTWRITEBYTECODE=1`.
The unchanged launcher imports the installed Python seed helpers; suppressing
their bytecode keeps the retained helper directory's link count stable.
An actual-helper regression invokes the existing argument-refusal route and
checks the parent binding and source bytes without performing seeding.
Before/after witnesses do not eliminate every privileged same-UID swap/restore race
for the launcher's fixed path or other operational inputs; the keeper's original
source/input witnesses remain independently required.

Init sends its original namespace and pidfd outward with kernel credentials.
Run joins a positive receiver-relative SCM PID, UID/GID, original pidfd NSpid and
the actual sender's PID-namespace device/inode through its original host proc.
Reported JSON PIDs never authorize a lookup, new pidfd, wait or signal. Original
pidfd polls bracket mapping and GO. Poll is liveness, not a consuming wait or
proof of every descendant.

In the reverse direction, ancestor credentials can encode PID0. The locked
Rustix UCred uses a nonzero PID type, so init requires SO_PASSCRED off before any
receive and the fixed run sender never emits credentials inward. GO authority
is the original private paired socket, sole writer, source, nonce and original
interval. Its two rights become OwnedFDs immediately, join the actual ancestor
namespace/pidfd role and stay close-on-exec. Unexpected/truncated/extra rights
are drained and explicitly closed. No raw received-FD adoption or first-party
unsafe code is used. The launcher receives DEVNULL input; control and capability
originals cannot escape into the shell through an executable descriptor.

## Exact process and negative evidence

The private init is the only consuming reaper. It peeks terminal children with
WNOWAIT, uses standard Child wait once for its direct launcher, and exact waitpid
for adopted children. It disarms the direct owner at its consuming observation
before later fallible checks. Actual ECHILD is required; EOF, a reserved group,
init death and VM teardown cannot replace it. Namespace signals require actual
PID1, a different original host namespace and unchanged original host proc.
They are never available to outer run.

Both deliberate negatives first require a kernel-observed same-namespace timeout
executable and the exact initial readonly Compose ps invocation. A missed,
foreign, unknown or mutating client leaves the case unproven. Namespace STOP is
then followed by WSTOPPED plus WNOWAIT on the exact owned launcher. The original
proc-task directory and executable stay held through a stopped-state command
recheck; an exited/reused or changed client leaves the trial unproven. The original
five-second client ceiling is checked after scan, after stopped-state validation
and checked closes, and at the Stopped send; none of those steps restarts it.
Cancel makes run safely self-raise a real SIGTERM on its original owner, then forwards it to
its original init pidfd. This tests a self-raised native signal, not external
operator delivery. Timeout keeps the known stopped namespace under the actual
900-second work clock, then performs TERM/CONT/KILL and all reaps. It does not
substitute a shortened timeout. Expected negatives remain native143/native124;
they never become native0.

The keeper separately observes daemon actors, fixed selections, actual SQL/ledger,
grants, seed and registration state, trusted TLS and original files. Native PID
namespace settlement does not authorize cleanup of an unknown Docker actor or
prove the Docker event stream had no gaps. Known fixture CID/network cleanup
and actual VM lifecycle remain separate mandatory observations.

## Closed output and ownership limits

The guest compares Caddy's added capabilities with the exact current Docker
inspection value `["CAP_NET_BIND_SERVICE"]`; the normal create argument remains
`--cap-add NET_BIND_SERVICE`. An owned Docker 29.8.2 daemon/client experiment
using the pinned Caddy image exposed the inherited bare-name comparison defect
before Caddy start. Extra, wrong, missing or noncanonical capability observations
remain refusals. The frozen Python reference is unchanged. This adapter
correction does not change the actual capability grant, mount/UID policy, TLS
validation, retained-file witnesses or original time budget.

One bounded JSON report contains schemaVersion, experiment, trial, passed, phase,
failure and facts. The complete closed inner failure enum propagates unchanged
unless an earlier outer failure already owns the result. The facts contain fixed
booleans/counts, nullable actual signed statuses, original tool and stream hashes,
elapsed work, bounded adopted statuses,
EOF and checked-close observations. No raw command output, argv, environment,
credentials, socket path, private configuration or arbitrary nested dictionary is
published. Hash fields must be canonical lowercase SHA256 strings or null; status
ranges and the retained adopted-status vector are validated before projection.
Refusal counters remain truthful even above their work cap. A stream digest is
available only after EOF with every consumed byte hashed and within the cap; late
or over-cap reads never produce a prefix/suffix digest labeled as complete.
Missing status is null, never zero. Earlier report bytes are passive;
the actual final status includes original owner/signal/umask/output fences.

Body and final stream reads have explicit pre/post fences, including EOF. Phase
receipt is checked again after recvmsg and returned rights close before a late
refusal. Original identity mismatch is recorded before secondary close failure.
All known originals attempt close after refusal. Borrowed stdin flags and the
explicit CHLD registration have separate restoration facts. Standard-library exec,
DEVNULL and duplicated standard-descriptor closes remain an opaque SDK boundary;
they are not described as checked original closes. A valid result is followed by
natural wrapper terminal status and both pipe EOF observations inside the same
original result-phase ceiling, checked before and after those observations.
Only then can the natural-join fact become true. Later physical group retirement,
reap, drain and checked closes still run under original N; they cannot promote a
late join to success or grant another result timeout.
The receiver drains queued authenticated phase packets before treating an exited
wrapper as a missing result. Queue emptiness is observed after terminal status;
an interrupted receive retries under the same caller deadline. Each packet keeps
its original nonce, credential, rights, ordering and result-phase checks, and a
valid result ends the drain before peer EOF. Without an admitted final result,
inner fact fields remain defaults rather than observations of launcher startup
or inner cleanup. A partial failed report cannot establish those events.
No production panic/unwind guarantee is made. The inert test fixture explicitly
restores flags, closes registered originals and finishes its owner on setup failure.

## Native guest network profile

The native guest uses one owned ordinary IPv4 bridge with
`com.docker.network.bridge.enable_ip_masquerade=false`. Its closed inspection
requires `Internal=false`, IPv6 disabled and exactly the two selected options:
the original bridge name and the string `false` for masquerading. The container
gateway must equal the canonical admitted IPAM gateway. Internal bridges, an
empty gateway, enabled masquerading and additional routing options refuse.
The frozen Python guest retains its earlier internal-network profile.

This deliberately changes the disposable network profile so the unchanged
launcher and seed helper can use their actual host-loopback backend at
`127.0.0.1:3000`. At the exact Docker engine commit
[`8af9fe3a36bab3e039862a2ab1cef1880c9b4d03`](https://github.com/moby/moby/blob/8af9fe3a36bab3e039862a2ab1cef1880c9b4d03/daemon/libnetwork/drivers/bridge/bridge_linux.go),
internal endpoints omit a gateway; gateway selection controls NAT publication
in [the port-mapping implementation](https://github.com/moby/moby/blob/8af9fe3a36bab3e039862a2ab1cef1880c9b4d03/daemon/libnetwork/drivers/bridge/port_mapping_linux.go).
Direct container-IP TLS does not establish host-loopback publication. The
fixture adds no forwarding relay or alternate launcher/readiness path.

All three existing default-drop hooks remain byte-identical: host output,
forwarding and the pinned physical interface's netdev egress. They are installed
before application or Caddy actors start and remain through guest exit. Only
loopback, the sole owned bridge and the original SSH control exception are
admitted in their existing roles; Docker's later forwarding rules do not
override an earlier drop. The actual guard probe, packet capture, actor/network
joins and final cleanup remain mandatory. An ordinary bridge is not authority
for physical egress or a production-network equivalence claim.

The exact local Docker 29.8.2 daemon/client and pinned Caddy image were acquired
for a comparison, but the emulated provider refused container startup at its
nested cgroup boundary. That trial supplies no HTTP, installed-firewall or
hosted-failure-cause proof. Policy regression checks and source analysis leave
the fresh signed four-scenario runtime required.

## Expected application replacement rename

The fixed launcher briefly enables registration for seeding, then recreates the
application with the installed policy. Pinned
[Compose 5.6.0](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/reconcile.go)
creates that replacement under a temporary name, retires the previous app and
renames the replacement before starting it. The observer joins this one rename
to the freshly inspected current app, exact project/service/run/image labels,
the sole retired old app, its CID prefix and the inspected canonical name.
The final current-app trace must be `create, rename, start`; missing, duplicate,
late, foreign, incorrectly named or other-service renames refuse. Existing
kill/stop rules and every other actor, PostgreSQL and inventory check remain.
Event data grants no command, signal or cleanup authority.

An owned Docker 29.8.2 create/rename experiment records the actual event shape
without starting its container, consistent with the pinned
[daemon rename implementation](https://github.com/moby/moby/blob/8af9fe3a36bab3e039862a2ab1cef1880c9b4d03/daemon/rename.go).
Its image is an inert pinned Caddy surrogate and the old CID context is synthetic;
it does not execute Compose, establish the hosted failure's exact inner branch or
prove the full app replacement. The frozen Python observer remains unchanged.
The current failed positive receipt records successful native execution followed
by an independent daemon-observation refusal. It stays negative evidence; a fresh
complete signed runtime remains required after the observer correction.

Capture retirement failures have a closed diagnostic checkpoint vocabulary. If
the watcher supplies the first local failure, the receipt identifies its live,
bound, EOF, stop, wait, status, short-capture or close check. A nonempty capture
may additionally receive a fixed first-header class: ARP, IPv4, IPv6, direct
ICMPv6 router advertisement, other or unknown. The small checked classic-PCAP
header classifier is diagnostic only: it establishes no origin, direction,
checksum or complete-packet validity and never admits captured traffic. No raw
bytes, addresses, names, timestamps or packet payload are exported.

IPv4 headers may receive one of seven narrower fixed hints: ICMP, IGMP, TCP,
UDP, DHCP, NTP or DNS. The last three identify selected port combinations,
not a validated application protocol. Only captured, bounded headers are read;
the fixed snapshot may omit the remaining payload. Unsupported, ambiguous,
fragmented or malformed headers retain the generic IPv4 class. These hints add
no direction, peer or timing evidence. The failed 900-second trial that reported
`capture-data-ipv4` remains unclassified beyond that Ethernet header class.

Earlier body or cleanup errors retain their original checkpoint. The matching
Rust outer decoder admits only the new literal failure labels; passing receipts
still require `complete`. This changes no wire fields or verdict requirements.
The empty-capture rule remains raw status zero, exactly 24 stdout bytes and at
most 8,192 stderr bytes; the 1 MiB capture limit, 4,096-packet ceiling, 128-byte
snapshot, waits, retirement grace, firewall and original deadlines are unchanged.
The earlier 900-second receipt remains negative: its capture failure was not
exported precisely enough to identify the cause. New diagnostics do not turn it
into evidence of packet contents or successful containment observation.

## Built-in none-network lifecycle

The fixed validator and migration containers use Docker's built-in `none`
network. Pinned
[Compose creation](https://github.com/docker/compose/blob/42f48072bbf92ee9b0e43f9fdf2008d03546e7ca/pkg/compose/create.go)
does not mark that mode `NetworkDisabled`. Pinned
[Docker allocation and endpoint handling](https://github.com/moby/moby/blob/8af9fe3a36bab3e039862a2ab1cef1880c9b4d03/daemon/container_operations.go)
therefore emits connect/disconnect events for that built-in network, even though
its driver is `null`. The owned fixture bridge has a different network ID.
Requiring every network event to carry only the bridge ID rejects this expected
transition. This is a deterministic source counterexample; the failed hosted
summary does not expose the exact event or inner refusal branch.
An owned pinned-Docker inspection confirms the initial built-in profile without
creating or starting an inner container. It does not execute network events or
establish a running PostgreSQL membership observation.

The observer binds the built-in network's inspected identity and profile before
the launcher and checks it again during final observation. Its event exception
is restricted to the already identified validator and sole migration actor,
with one ordered connect/disconnect pair each. Docker's fixed lifecycle connects
after creation and disconnects during cleanup before the die event. Complete
actor histories and all existing bridge, PostgreSQL and inventory checks remain
required. The built-in network grants no command, signal or retirement authority.

Daemon-observation failures use ten fixed operation checkpoints to distinguish
actor identity, inventory, validation input, event shape, actor classification,
rename, action history, network history, PostgreSQL history and final traces.
Only the retained first failure selects a label; earlier causes keep precedence,
and successful observation keeps the existing phase checkpoint. Labels contain
no event values, IDs, names or command text, add no wire fields, and cannot be
used as a passing receipt's checkpoint. The frozen Python observer stays unchanged.
A fresh signed four-scenario runtime remains required after this correction.

## Validation state

The earlier fixed run/init body passed GNU/musl compilation and Clippy, nine
portable Darwin contract tests, and all 20 native maintenance methods in
[Linux CI 40239549127](https://github.com/swiftraccoon/simplestChat/actions/runs/40239549127).
Those Linux methods include actual process/FD, inherited-clock and late-observation
checks; they do not execute the whole PostgreSQL/Caddy launcher transaction.

The new guest adds actual signed-material/TLS corpus tests, strict source/render
admission, an inert `images.json` handoff with root ownership and mode `0600` under
its original root parent with mode `0700`, and fixed `sc_maintain_` firewall tests.
It retains the 510-second setup cap, requires 1170 seconds before its single
1050/60 native call, and keeps the 1800-second guest interval with 120 reserved.
Known process effects are adopted before later capture/close errors can hide
them; cleanup failure and later time/signal status cannot erase the first cause.
Native producer tests use inert commands and parsed ELF/archive fixtures; they
do not establish an actual cold musl/OpenSSL build or signed execution.

Local validation of the integrated change passed the complete Rust quality gate
(workspace all-target/all-feature Clippy, generated contracts and warning-free
private Rustdoc). The Darwin Ops library passed 489 tests, including all 43 guest
tests, with 11 existing ignored fixtures. All 53 hosted producer/signer tests
passed after the archive and whole-fixture integration repairs; the generic
producer's nine native supervision tests also passed. The default TLS-free
`xtask` graph cross-checked for Linux musl and excluded OpenSSL/X.509/ASN.1.
The 30 frozen Python keeper tests and offline Ansible lint passed. These checks
leave the actual Linux native build and fresh signed VM trials unverified.

The first exact CodeQL trial reported three public-certificate vector removals
as cleartext logging. Independent flow/model review found the pinned generated
`Vec::remove` logging-sink model and certificate-name heuristic; these paths
contain no logging operation or private key. The code now consumes checked
one-element certificate arrays instead of mutating those vectors. No query,
policy exception or secret-handling rule was weakened; fresh exact-revision
analysis remains required after this ownership simplification.

The latest four-case runtime of the frozen guest passed all 33 Ops cases but
failed at `render` before native execution. Its retained I/O refusal is distinct
from the source-proven inert-file collision and Caddy-helper table-prefix mismatch.
The [runtime guide](maintenance-launcher-runtime-proof.md) records the actual
failed trial and evidence limits. Fresh CI, final static/signed inputs and all
four real guest trials are still required for the new guest route. The earlier
Python namespace and Caddy-only runs do not certify this complete transaction.
