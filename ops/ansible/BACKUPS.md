# Encrypted offsite database backups and restore drills

`backup-offsite.yml` schedules encrypted uploads of the current nightly
PostgreSQL archive and a weekly restore of the exact uploaded snapshot. Upload
success and restore success are separate evidence. Restoration always uses a
new disposable, network-isolated PostgreSQL container; the public database is
never a restore target.

The workflow uses [restic's encrypted SFTP repository](https://restic.readthedocs.io/en/stable/030_preparing_a_new_repo.html#sftp).
The playbook pins the Debian 13 amd64 package `restic=0.18.0-1+b4`, verified in
[Debian's package index](https://packages.debian.org/trixie/restic) on 2026-09-30.
SSH uses only the supplied identity and known-hosts file, strict host-key
checking, no agent, no inherited SSH configuration and no password prompts.
Repository passwords are read from a private file, never placed in command
arguments or environment variables.

## Required configuration

Prepare the public host, apply `monitoring.yml` with `scmon_enabled: true`, and
install the local timer with `backup.yml` and `scpub_backup_enabled: true`.
The restore verifier checks the application's schema, migration ledger and
monitoring schema together. External workflow importing can remain disabled.
At least one new nightly archive must exist before its offsite upload can run.

Supply an initialized restic repository on a separate SFTP host, an SSH identity
already authorized there, pinned SSH host keys and that repository's password.
These are explicit prerequisites: the automation does not initialize or guess a
repository, enroll unverified host keys or create storage credentials. Keep the
password and SSH recovery material securely available outside the application
VPS; losing the repository password prevents recovery.

Add only nonsecret settings and private controller file paths to the ignored
inventory. The three input files must remain outside tracked source. For example:

```yaml
scpub_backup_enabled: true
scmon_enabled: true
scpub_backup_offsite_enabled: true
scpub_backup_offsite_host: backup.example.net
scpub_backup_offsite_user: simplestchat_backup
scpub_backup_offsite_port: 22
scpub_backup_offsite_repository: /repositories/simplestchat
scpub_backup_offsite_identity_file: /private/operator/backup-identity
scpub_backup_offsite_known_hosts_file: /private/operator/backup-known-hosts
scpub_backup_offsite_password_file: /private/operator/backup-password
scpub_backup_restore_archive_mib: 64
scpub_backup_restore_memory_mib: 512
scpub_backup_restore_data_mib: 256
```

The playbook transfers the supplied files without logging their contents and
installs them as root-owned 0600 files beneath `/etc/simplestchat-backup` (0700).
It installs both timers but does not perform a deployment or restore into the
application database:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/backup-offsite.yml \
  --limit public_vps
```

Upload runs daily at 04:30 UTC, after the 04:00 local backup schedule. Restore
runs Sunday at 06:00 UTC, after the upload service's maximum runtime. Both add
up to five minutes of randomized delay and persist missed timer events across
reboot. They use the canonical workload lock;
a release, benchmark, backup or unfinished operation blocks admission. A failed
invocation remains failed; it is not silently retried as another experiment.

To run either scheduled action immediately, use its bounded service:

```sh
sudo systemctl start simplestchat-backup-upload.service
sudo systemctl start simplestchat-backup-restore.service
```

## Archive identity, bounds and evidence

Uploads accept only a verified nightly receipt whose archive is at most
36 hours old. The receipt records the archive hash and size, the exact
PostgreSQL image and the applied migration versions/checksums. Restic encrypts
both the archive and its receipt. The helper records the full snapshot ID and
repository identity in `/var/lib/simplestchat-monitoring/offsite-latest.json`;
selection never uses an ambiguous `latest` lookup against arbitrary repository
contents. No remote snapshots are deleted or pruned by this workflow. Configure
storage-side retention, capacity and deletion protection separately.

A restore retrieves the selected receipt and archive through `restic dump`.
A kernel file-size ceiling bounds each download. The helper checks private file
ownership, type and mode, the receipt schema, archive size and SHA256 before
running PostgreSQL. The isolated restore uses the exact recorded local image
with `--pull never`, restores every archive section, compares the migration
ledger, checks table/constraint/role invariants and runs `pg_amcheck`. It deletes
only its uniquely labeled container and downloaded plaintext copies. It advances
`restore-verified.timestamp` only after successful validation and cleanup.

The default archive download limit is 64 MiB. It is configurable from 1–4096 MiB;
this is an archive-size limit, not a claim about the uncompressed database size.
The disposable database uses tmpfs with a 256 MiB data ceiling and 512 MiB memory
ceiling by default. Memory may be configured up to 64 GiB but cannot exceed half
the host's memory in the playbook; data must leave at least 256 MiB for the
PostgreSQL processes. Choose limits for the actual restored data and repeat the
drill after growth. An archive, expanded database or process that exceeds its
bound fails verification; it does not produce a success timestamp. Downloads
reserve their configured maximum plus 1 GiB of local free space.

Each operation retains private command evidence under
`/var/lib/simplestchat-monitoring/offsite/{upload,restore}.*`. Success writes an
`outcome.json`; restore also retains its isolated-container result. These records
are not automatically pruned. They contain archive identities, operational
metadata and potentially private error output; keep them on the private host.
A command timeout stops the owned local process group. The next restore refuses
an unrelated retained restore container. Each offsite operation first records an
unfinished canonical ownership journal, so an abrupt worker termination blocks
release and benchmark admission. The service's `ExecStopPost` and the next offsite
invocation recover that exact attempt: they verify the recorded container name,
label, image and full ID before removing it, remove only recognized private
download files, and finalize the journal only after cleanup succeeds. Recovery
does not turn the original failure into a successful upload or restore.

To recover without starting a new upload or reading repository credentials, use
the installed worker:

```sh
sudo /usr/bin/python3 -E -s -B /usr/local/libexec/simplestchat-public/backup_offsite.py recover
```

An ownership mismatch remains blocked with private evidence for investigation;
the helper never deletes every container sharing a label. Release backups from
before the recorded-ledger contract are rejected rather than interpreted through
an older format.

The local backup freshness gauge validates the local receipt and archive. Its
success does not prove an upload. Offsite upload evidence and the isolated
restore marker are independent, and a failed upload cannot advance the restore
marker. Monitor failed systemd units and the existing restore-age alert; retain
and inspect the private offsite outcome when confirming a backup.

## Restoring a chosen snapshot after loss of local selection state

On a prepared recovery host, supply the same trusted repository configuration
and password, and preload the exact PostgreSQL image identified by the archive.
Choose a full snapshot ID and archive timestamp from the repository's reviewed
inventory. The same verifier supports an explicit selection without relying on
the lost application VPS's state:

```sh
sudo systemd-run --unit=simplestchat-offsite-drill \
  --property=Type=exec --property=RuntimeMaxSec=2100 --property=TimeoutStopSec=45 --wait \
  /usr/bin/python3 -E -s -B /usr/local/libexec/simplestchat-public/backup_offsite.py \
  restore --snapshot FULL_64_CHARACTER_SNAPSHOT_ID --stamp YYYYMMDDTHHMMSSZ
```

This performs an isolated recovery drill. Promoting a restored database into a
live deployment is a separate explicit operation with its own consistency and
service-cutover plan. The archive's successful listing, encrypted upload and
full restore are three distinct checks; none substitutes for the others.
