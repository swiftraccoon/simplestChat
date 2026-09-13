# Reproducible VPS setup

Use Docker for the application and Ansible for the host. This automation targets
a dedicated **Debian 13 x86_64 VPS with systemd**. It installs pinned Docker,
checks out an exact source commit, and installs separate, opt-in image-build and
benchmark services. It does not install Rust or Node on the host, publish a chat
service, configure a database, or change SSH authentication.

Docker access is root-equivalent. Host orchestration runs as root; application
and load-generator containers run as UID/GID 10001 with read-only filesystems,
dropped capabilities and no additional privileges. No user is added to the
Docker group. Docker manages its normal bridge/netfilter rules; this automation
does not replace your firewall or daemon policy. Existing conflicting runtimes
and custom Docker API listeners require review instead of automatic removal.

## Prepare a host

On your controller, install Python 3.12+ and create an isolated environment:

```sh
python3 -m venv ops/ansible/.venv
ops/ansible/.venv/bin/pip install -r ops/ansible/requirements.txt
cp ops/ansible/inventory.example.yml ops/ansible/inventory.local.yml
```

Edit the local inventory with your host, SSH user, absolute key path and full
40-character source commit. The copied `inventory.local.yml` is ignored;
never commit private keys, runtime secrets or real host details. Verify the
host's SSH fingerprint and establish trusted SSH access first. Host-key checking
stays enabled and the controller's SSH agent is not forwarded.

For a fresh VPS, explicitly enable `scbench_upgrade_packages` and
`scbench_reboot` for the initial maintenance run. Both default off. Reset them
afterward; reapplication should not unexpectedly perform OS maintenance.

Run from the project root:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/site.yml
```

Provisioning does not build images or run benchmarks. Services are disabled at
boot. Reapply after reviewing configuration changes. Active workloads and
unfinished container cleanup block provisioning, including tagged runs.
`--syntax-check` is offline; Ansible check mode on an unprovisioned host cannot
fully validate dependencies that have not been installed yet.

## Build once, then reuse the image

On the VPS:

```sh
sudo systemctl start simplestchat-image-build.service
sudo journalctl -fu simplestchat-image-build.service
```

The first build can take tens of minutes. It uses the existing multi-stage
Dockerfile's `production` and `loadtest` targets, preserving its native dependency
checks. Compilers remain in build layers, not the host or production runtime.

Each attempt keeps logs and host/build identities under
`/srv/simplestchat-bench/artifacts/<commit>/build.*`. A successful build creates
`images.json` containing exact local content-addressed image IDs. Re-running the
builder verifies and reuses them; missing images fail instead of silently
replacing the tested artifact. Use a new source commit for the next version.
No image is pushed to a registry by this automation.

Build inputs pin base images and application dependencies, but OS package
repositories can change between builds. Retain the resulting images for exact
reruns; matching Dockerfiles alone do not guarantee byte-identical rebuilds.
Registry publication is a separate release action. Use a registry manifest
digest once published, or `docker image save`/`load` to transfer retained images.

## Run a private baseline

```sh
sudo systemctl start simplestchat-benchmark.service
sudo journalctl -fu simplestchat-benchmark.service
```

Defaults are 10 synthetic publishers in one room, one media worker, five seconds
of ramp-up, ten seconds of warmup and a 60-second measured interval. The server
uses Docker's `none` network; the generator shares only that owned namespace.
No HTTP or UDP ports are published. A scoped metrics credential exists only for
that run. The workload uses guest rooms without PostgreSQL, TLS or TURN.

Results live under `/srv/simplestchat-bench/results/run.*/workload`. A pass requires
current per-client delivery coverage, zero final room/session counts, observed
zero process exits without OOM, and completed owned-container cleanup. Reports
include launcher checksums; bounded logs remain after containers are removed.
A supervisor cleanup hook
handles interrupted launchers; uncertain daemon-side creation remains explicitly
unfinished and blocks subsequent runs. Do not remove that record to bypass it.
Do not reboot an active run: the live ownership record is under `/run` and does
not survive a reboot. Host failures require inspection of retained artifacts and
containers before another workload. Likewise, inspect Docker after an interrupted
image build; stopping its client does not prove daemon-side work has finished.

Change `scbench_workload` in your inventory and reapply while idle. Specify all
fields when overriding the mapping: `clients`, `rooms`, `workers`, `rampUp`,
`warmup`, and `duration`. The runner bounds workloads and preserves admission
limits. It supports up to 30 clients, four rooms/workers and 180 measured seconds.
Every invocation gets fresh artifacts; failed attempts are not overwritten or
automatically retried. Build and benchmark execution share an exclusive lock.

This establishes synthetic forwarding and cleanup, not browser quality, WAN
capacity, authenticated workloads or an optimal production configuration.
Server/generator cgroup samples are separate; Docker memory accounting is not
process RSS. The generator shares the VPS, and hypervisor CPU steal may vary.
Compare repeated, alternating runs before choosing a configuration. Additional
workers do not split a single room's router across CPUs.

## Deploy and maintain

The existing [Compose deployment](../../docs/deployment.md) remains the public
runtime path. Set `SIMPLESTCHAT_IMAGE` to a retained immutable image and deploy
with `--no-build`; configure HTTPS, secrets, database and media ports separately.
The benchmark setup intentionally does not make those deployment decisions.

Review package pins in `group_vars/benchmark_hosts.yml` when upgrading Docker.
Take a provider snapshot before maintenance and retain image/result archives
before replacing the VPS. This playbook does not prune images, erase results,
roll back database changes, or promise reproducibility of an unrecorded host.

Offline checks require ShellCheck and `jq` in addition to the controller tools:

```sh
export PATH="$PWD/ops/ansible/.venv/bin:$PATH"
export ANSIBLE_CONFIG="$PWD/ops/ansible/ansible.cfg"
ansible-lint --offline --strict ops/ansible
ansible-playbook -i ops/ansible/inventory.example.yml ops/ansible/site.yml --syntax-check
python -m unittest discover -s ops/ansible/tests -v
```
