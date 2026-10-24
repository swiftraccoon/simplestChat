# Maintenance prerequisites in a disposable guest

The optional **Maintenance prerequisites KVM experiment**
[manual workflow](../.github/workflows/maintenance-prerequisites.yml) measures the
pinned Caddy image and finite PID-namespace topology in two independent disposable
Debian 13 KVM guests on free Ubuntu runners. The Caddy job measures natural local
TLS/storage behavior; the namespace job observes its fixed inert process catalog.
Neither job establishes the other's claim or a complete maintenance operation.
The workflow accepts no artifact, token, server, inventory, target host or arbitrary
command. It has only `contents: read` and is not a required CI check. Production
maintenance code and installed callers remain unchanged. No Cargo build, signed
Ops bootstrap or authenticated artifact role is part of these experiments.

## Run and evidence status

The Caddy sources are [the controller](../build/caddy_probe_vm.py),
[guest](../build/caddy_probe_guest.py), [passive analyzer](../build/caddy_materials.py)
and their three `test_caddy*.py` suites. The focused suite retains the original 51
inert cases and adds repository/negative-metadata regressions (60 total). The
companion [namespace prototype](maintain-namespace-prototype.md) has two helpers
and 14 inert protocol/clock/FD/workflow cases. Together the focused suites have 74
local file/crypto/fake-process tests under the maintained strict configuration.
They execute no actual helper role, namespace, KVM, Docker or natural TLS.

Actual Caddy issuance/reuse, DNS containment, the real 8 MiB validator and the
namespace 11-case/14-trial kernel catalog remain **unrun** until separately
recorded manual jobs. The unchanged operational 900-second launcher is also
unrun. Neither source/inert checks nor one experiment establish complete
maintenance, application health, caller cutover or the other experiment's result.

Use the manual workflow on `main` in the canonical repository. It installs the
existing hash-pinned Python controller requirements and free KVM/SSH tools.
The Caddy job uses explicit fixture N=1800/M=120/G=60. The namespace job retains
its fixed 1800-second original envelope, body 1680/finalization 120, with subordinate
guest/case durations clipped from that same clock. These are separate experiments;
neither resets or lends a deadline to the other. They are not native product
defaults or measured success guarantees. Each 45-minute job allows host prerequisite
setup plus its 30-minute controller; each experiment step has 35 minutes. Host tool
installation occurs before fixture entry and does not enlarge either original end.

A prepared dedicated Linux amd64 KVM runner can use these repository commands.
Run as the unprivileged runner user with access to `/dev/kvm`, QEMU, cloud-image
utilities, OpenSSH, curl and the existing controller requirements. The output
must be a new canonical absolute path with an existing parent:

```sh
caddy_parent="$(mktemp -d)"
python3 -m venv "$caddy_parent/controller"
"$caddy_parent/controller/bin/python" -m pip --isolated install \
  --require-hashes --only-binary=:all: -r ops/ansible/requirements.txt
"$caddy_parent/controller/bin/python" -B build/caddy_probe_vm.py \
  --project-root "$PWD" --output "$caddy_parent/proof" \
  --timeout-seconds 1800 --finalization-seconds 120 \
  --guest-finalization-seconds 60
```

The actual outer entry clock includes cloud download, authenticated generated
loopback SSH, cold guest APT/Docker setup, image acquisition, all observations,
QEMU/input/work retirement and final stdout/handler/umask fences. M=120 is reserved
for outer finalization. Existing child TERM 5/reap 5 debt stays inside body. G=60 is
an explicitly requested subordinate guest cleanup window; the guest receives
only the measured remaining controller body duration. Handoff delay is charged
by the original controller; distinct machines' monotonic epochs are never
compared. Cold setup, signals, insufficient remainder or blocked I/O can refuse.
The existing QEMU scaffold's 1800-second ceiling is unchanged. Safe blocking
kernel/SSL/filesystem/SDK operations have no per-syscall hard-wall guarantee.

## Separate namespace experiment

The same prepared KVM runner can execute only the fixed namespace probe with a
fresh output path. Its only controller argument is the output directory; there is
no command, case, host, image or timeout selector:

```sh
namespace_parent="$(mktemp -d)"
python3 -m venv "$namespace_parent/controller"
"$namespace_parent/controller/bin/python" -m pip --isolated install \
  --require-hashes --only-binary=:all: -r ops/ansible/requirements.txt
"$namespace_parent/controller/bin/python" -I -B build/maintain_namespace_probe.py \
  --output "$namespace_parent/proof"
```

The [namespace guide](maintain-namespace-prototype.md) specifies the original
namespace/pidfd/SCM credential joins, two wrong-capability refusals, fixed GNU
timeout topology, all adopted reaps to ECHILD and the unchanged host-proc role.
Returned stdout/stderr/channel/capability owners, signal/mask restoration, actual
wrapper reaps, original final fences and QEMU exit 0 are required. Python's internal
DEVNULL/exec duplicates remain an explicit SDK boundary; Rust fd0 transport and
the complete operational launcher are not exercised. Missing kernel capability,
proc visibility or utility evidence refuses the experiment without a fallback
host/PID/command selector or a larger budget.

Positive namespace `summary.json` is uploaded only after job success. A separate
always-attempted artifact retains only `failure-summary.json` when available.
Both experiments use the same closed negative-metadata policy below; no raw
stream, guest dictionary, work directory or credential leaf is eligible.

## Fixed Caddy source and network boundary

The only Caddy image matches the reviewed
[image setting](../ops/ansible/group_vars/benchmark_hosts.yml) and
[image recipe](../build/caddy-image.Dockerfile): version2.11.7, build revision
`73c81fdbfe625d3df3989ba807064c81fbe2c071`, pinned digest
`sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0`.
The actual image platform/config/RepoDigest, Caddy version, original ELF hash,
Go dependency build-info and registered modules must be observed. CertMagic must
be0.25.6, acmez3.1.7, no build replacement is admitted, and the actual registered
`tls.issuance.internal` module must exist. An image name or source `go.mod`
alone does not establish these runtime observations.

Cloud/APT/image preparation may contact its fixed package/registry services;
it starts no Caddy executable. All Caddy and validator roles then run behind
owned guest nftables rules. OUTPUT default-drop covers dockerd/resolved DNS
forwarding in the host namespace; FORWARD default-drop restricts container
routing. A second physical netdev egress hook permits only the actual QEMU SSH
control peer's replies and ARP. There is no blanket established-connection bypass.
An internal Docker bridge alone is not an exhaustive egress guard.

A bounded known tcpdump child observes every non-control physical frame, excluding
only that SSH peer and its ARP. Overflow, early exit, missing completion or any
non-control packet prevents success. After guard installation, one inert UDP
marker targets the RFC5737 documentation address192.0.2.1:53 and must increment
the blocked OUTPUT counter. No public CA endpoint is configured or allowed.
Actual confinement still requires the manual run. Both rulesets
remain through guest exit and deferred resolver work. The exact owned VM's
verified zero exit retires them; no successful nft-delete result is claimed.
Unsupported kernel hooks, unexpected traffic or lost SSH refuse the experiment.

Each Docker creation latches uncertainty before dispatch. Only raw-zero complete
natural body output grants an exact immutable container ID. That effect is owned
before fallible evidence flush/close. Nonzero, truncated or unknown creation
never authorizes cleanup by generated name or a positive verdict. All known
container IDs and admitted local peers are attempted after the first failure;
there is no engine-wide prune or foreign-resource cleanup.

## Natural material and TLS observations

The fixture uses `maintain.localhost` and the exact unchanged root Caddyfile,
normal data/config and readonly config/browser-asset mounts, UID/GID10001,
readonly root and the normal bounded tmpfs/capability/log profile. Minimum
256MiB/0.5CPU and loopback-only published ports are deliberate disposable-fixture
choices, not production-host equivalence. Actual inspect fields must match the
selected arguments. No manual baseline certificate, TLS/issuer directive or
certificate mount is injected. The upstream can remain inert: fixed `/metrics`
HTTP404 establishes TLS only, never backend, application or DB success.

The actual selected Caddy must generate seven natural original files:

- `pki/authorities/local/root.crt` and `root.key`;
- `pki/authorities/local/intermediate.crt` and `intermediate.key`;
- `certificates/local/maintain.localhost/maintain.localhost.crt`, `.key` and `.json`.

All seven canonical nofollow files and original parent namespaces stay held.
Each material is at most64KiB; metadata at most16KiB; total at most448KiB.
UID/GID10001, mode0600, nlink1 and complete identity/content stamps excluding
access time must remain unchanged. Parent ownership/mode/inode are checked;
legitimate lock/instance/cleanup auxiliary writes are allowed. This is not a
whole-directory-membership seal or immunity to a transient same-UID swap and
restoration.

The passive analyzer uses mature cryptography signature APIs to check root,
intermediate and leaf links, key/SPKI correspondence, exact chain members,
constraints, sole DNS SAN, usages, actual UTC validity and renewal margins.
Metadata is duplicate-safe exact `sans` plus present-null `issuer_data`; OCSP
URIs are refused. Source defaults12h/7d/3600d are not forced lifetime equality;
actual lifetimes and margins are recorded. It is not a generic WebPKI verifier.

Only an exclusively created, byte-pinned copy of the public root enters the
**guest's** trust store. Its original FD/namespace remains held through checked
retirement. No private-key bytes leave the guest and developer-host trust is
untouched. Actual guest Python/cryptography/SSL versions are recorded. Python3.13
public verified and served chain APIs are required; the private runtime adapter
keeps the repository's Python3.12 typing baseline and validates returned bytes.
No private `_sslobj` API is used.

Real default guest trust and hostname verification must bind both observed TLS
chains to the held leaf/intermediate/root. A fresh normal Caddy process over the
same data/config must retain all seven original bytes/stamps and identical real
TLS chains. Wrong trust and wrong hostname must produce actual SSL refusal;
a deadline or cancellation is not a wrong-trust proof.

The actual existing network-none validator uses the unchanged config/env, user,
caps,256MiB/pids128/log limits and separate8MiB `/data` and `/config` tmpfs. Its
real wait status must be0. Two private copies remove the leaf key or corrupt
issuer metadata. Passive material admission must reject them; actual Caddy
refusal or regeneration is observed separately and never called original reuse.
No renewal-time, ARI/OCSP network or installed-maintenance temporal parity is
inferred from these tests.

## Closed failure metadata and retention

Each experiment's positive `summary.json` is uploaded only after its controller
returns success, including actual QEMU exit 0 and all final fences. A separate
`always()` step attempts only the exact `failure-summary.json`, even when setup
or an experiment step fails. Missing files are ignored without changing the
failed job. No glob, raw stream, guest dictionary, config, key, material payload,
SSH work directory or disk is retained by either artifact step.

The negative schema always says `passed:false` and `metadataOnly:true`. It carries
only the fixed experiment/controller checkpoint, strictly projected guest phase
or named case, closed exception families, actually returned signed SSH status
or null, bounded row byte count/SHA256, and separate cleanup/restoration/fence
facts. Unknown guest fields or error text cannot enter it. SDK refusal before a
status return remains null; its initialized/default status is never substituted.
The first observed refusal stays primary through cleanup and diagnostic output.
A matching fixed JSON line is attempted on stderr when a file cannot be retained.

The original output namespace and original end still fence metadata-file creation.
Before output admission, after a hard kill/deadline, or after a write/close fault,
the file can be absent or partial; parse failure means unavailable evidence. A
late output/fence error can leave earlier passive positive bytes, but the final
nonzero status invalidates them. Failure JSON never proves authentication,
resource settlement, health or operational maintenance success.

## Reviewed primary source bindings

The normative source revisions are [Caddy2.11.7][caddy-revision],
[CertMagic0.25.6][cm] and [acmez3.1.7][acmez-revision]. The27
versioned official file links below retain their reviewed SHA256 content bindings.
Tag links are convenient navigation; the exact revision and content digest are
the binding. This inventory is source evidence, not actual image/runtime proof.

- [caddyserver/certmagic/crypto.go][src01]
  SHA256 `d4677b92deec26f48afe901bdc714c8b0598340ee66c2eaa4833c4cd10f6530f` (13266 bytes).
- [caddyserver/certmagic/config.go][src02]
  SHA256 `f5810f859c19c481b0072539ce6ec22f5fccc19a107dd4fb43f05fac0017c65a` (56156 bytes).
- [caddyserver/certmagic/storage.go][src03]
  SHA256 `51855fb679da444049dcbf1cabf78d162f6e2b5d54c7f4e959ac69f3f70208bd` (13256 bytes).
- [caddyserver/certmagic/maintain.go][src04]
  SHA256 `e3157e439061dc2b86041c9146565fccc955c55a87d360b6214f253c193fe89d` (37582 bytes).
- [caddyserver/certmagic/certmagic.go][src05]
  SHA256 `db1f1f7338ee1987cea534b5cddb95e33b32eb37cb54d9a0540c2143df591b74` (17988 bytes).
- [caddyserver/certmagic/certificates.go][src06]
  SHA256 `e19941c61a3ba076adbab331693a0526db5c51e0fc109ac81cc88e4073f9e607` (28532 bytes).
- [caddyserver/certmagic/acmeissuer.go][src07]
  SHA256 `03009e31e73b54edeeae9095ae653921b56350a7380842f3d01147a4f0c7c730` (23877 bytes).
- [caddyserver/certmagic/go.mod][src08]
  SHA256 `51dd47e7c8d7ccc92424e93b537dbee562507a8152e6b2b5276b9f48b6a8e531` (498 bytes).
- [caddyserver/certmagic/LICENSE.txt][src09]
  SHA256 `b40930bbcf80744c86c46a12bc9da056641d722716c378f5659b9e555ef833e1` (11357 bytes).
- [caddyserver/caddy/go.mod][src10]
  SHA256 `62fdd786b2f9ced74c818132ebaeba320e25528ed316202902918afa73473b72` (8385 bytes).
- [caddyserver/caddy/modules/caddytls/automation.go][src11]
  SHA256 `fe8605bdac9aef2f8658b45afeabcd69b4b98dfcda535d89a9e411512729224e` (23569 bytes).
- [caddyserver/caddy/modules/caddytls/tls.go][src12]
  SHA256 `d63bb6f20bf51628617982ccb40b76bd56849feecb58a11a0a2414b367592ff8` (39057 bytes).
- [caddyserver/caddy/modules/caddypki/acmeserver/acmeserver.go][src13]
  SHA256 `cacbc1f405ba058207d9e7da527c8d0ce2e6ded84e263a612add1a31372979b0` (11405 bytes).
- [caddyserver/caddy/modules/caddypki/acmeserver/caddyfile.go][src14]
  SHA256 `4b1b992231b82759453593f8d436d9be3cb427734e972ea4874cc3f371a42610` (4011 bytes).
- [caddyserver/caddy/modules/caddypki/acmeserver/challenges.go][src15]
  SHA256 `5accd4c2c3a9b12bbe3544f53d38f6f3e79bda12969737f6a687a334cd61ad75` (1868 bytes).
- [caddyserver/caddy/modules/caddypki/acmeserver/policy.go][src16]
  SHA256 `f2598ce05cb42a2d3fdc334403d0cf8d13c14cb9bd74ceae617a2bed747372eb` (2903 bytes).
- [caddyserver/caddy/modules/caddypki/adminapi.go][src17]
  SHA256 `d11641289705458af0eec9839c4523b55658df4e2f08117c103fa74333195028` (7418 bytes).
- [caddyserver/caddy/modules/caddypki/ca.go][src18]
  SHA256 `6d21eea49a1205e515cb7852b01e567e335ba611b15253b94ac98d5c870b208b` (15487 bytes).
- [caddyserver/caddy/modules/caddypki/certificates.go][src19]
  SHA256 `421ef4107e0a83b85d9417c6cc1981912b5d8a4b0a8df3e121202ad7a0156cbc` (2270 bytes).
- [caddyserver/caddy/modules/caddypki/command.go][src20]
  SHA256 `9625423ee873561f0a00396116230c577f398ec5c4fe9c38f75b520a4c2c8ab1` (8495 bytes).
- [caddyserver/caddy/modules/caddypki/crypto.go][src21]
  SHA256 `6230da8cf63fccdfd800bdb5a2bc4bb991b5cd16d9fa2fa364c43b3ecb607d0f` (4810 bytes).
- [caddyserver/caddy/modules/caddypki/maintain.go][src22]
  SHA256 `c672e44381414c6f373bf972f2f0dc40bba6c13fdc6a1c0c01d45e6acbc835ac` (3160 bytes).
- [caddyserver/caddy/modules/caddypki/pki.go][src23]
  SHA256 `bf6a672d7bdc6ffdfff2bf4742d9d5e03d4f93436efa4983f97fde5ab16076b9` (4259 bytes).
- [caddyserver/caddy/modules/caddytls/internalissuer.go][src24]
  SHA256 `1b2a8198520e3f1adf7a8f4fecc04e0fd0a16077292e960f56cd0212c0b2da87` (5577 bytes).
- [caddyserver/certmagic/filestorage.go][src25]
  SHA256 `d8e379f2d5af989f14a16c6ebc0dd16154636bf9a874f80ab28e9eaa525c1fce` (14984 bytes).
- [caddyserver/certmagic/ocsp.go][src26]
  SHA256 `3ad75e98c9fdd8d040d223fd6ef4ed8ec477f1d99479fb5c288236a101b76f37` (10228 bytes).
- [mholt/acmez/acme/ari.go][src27]
  SHA256 `260f5e26dd20624594d3afde8ddde1a37822baaeaa087638f5a677e23cee6e8f` (11277 bytes).

[caddy-revision]: https://github.com/caddyserver/caddy/tree/72dd0fb067f6d7826c7f79907670ba4a713bfe37
[cm]: https://github.com/caddyserver/certmagic/tree/23f4deea4a5f1e933dbca570ae3d9ce87a6d1fe7
[acmez-revision]: https://github.com/mholt/acmez/tree/8d3536e7ba1bc2fe63bb26b88d49c8772eb0438c
[src01]: https://github.com/caddyserver/certmagic/blob/v0.25.6/crypto.go
[src02]: https://github.com/caddyserver/certmagic/blob/v0.25.6/config.go
[src03]: https://github.com/caddyserver/certmagic/blob/v0.25.6/storage.go
[src04]: https://github.com/caddyserver/certmagic/blob/v0.25.6/maintain.go
[src05]: https://github.com/caddyserver/certmagic/blob/v0.25.6/certmagic.go
[src06]: https://github.com/caddyserver/certmagic/blob/v0.25.6/certificates.go
[src07]: https://github.com/caddyserver/certmagic/blob/v0.25.6/acmeissuer.go
[src08]: https://github.com/caddyserver/certmagic/blob/v0.25.6/go.mod
[src09]: https://github.com/caddyserver/certmagic/blob/v0.25.6/LICENSE.txt
[src10]: https://github.com/caddyserver/caddy/blob/v2.11.7/go.mod
[src11]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddytls/automation.go
[src12]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddytls/tls.go
[src13]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/acmeserver/acmeserver.go
[src14]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/acmeserver/caddyfile.go
[src15]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/acmeserver/challenges.go
[src16]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/acmeserver/policy.go
[src17]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/adminapi.go
[src18]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/ca.go
[src19]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/certificates.go
[src20]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/command.go
[src21]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/crypto.go
[src22]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/maintain.go
[src23]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddypki/pki.go
[src24]: https://github.com/caddyserver/caddy/blob/v2.11.7/modules/caddytls/internalissuer.go
[src25]: https://github.com/caddyserver/certmagic/blob/v0.25.6/filestorage.go
[src26]: https://github.com/caddyserver/certmagic/blob/v0.25.6/ocsp.go
[src27]: https://github.com/mholt/acmez/blob/v3.1.7/acme/ari.go
