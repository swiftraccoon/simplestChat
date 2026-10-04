# Move an existing site to a fresh VPS

`build/migrate.py` moves the supported public deployment between two explicitly
selected Debian 13 x86_64 hosts. It preserves the database and deployment secrets,
uses the exact signed image from successful main CI, and prepares the destination
while the source remains live. The source is stopped only for the final database
archive and cutover. No image is built on either server.

The destination must have no application containers or database files. Existing
prepared configuration is accepted only for the selected destination origin, and
existing secrets must exactly match the source. A different destination database
is never replaced. The command verifies physical machine identities, so two SSH
aliases cannot accidentally select the same machine.

## Prepare the two inventories

Keep the current source inventory and create a separate ignored destination
inventory using the [public configuration guide](PUBLIC.md). Both must explicitly
set `ansible_host`, `ansible_user`, an absolute `ansible_ssh_private_key_file`,
`scpub_enabled: true` and the correct `scpub_domain`. Establish trusted key-based
SSH access first. Non-root users need passwordless sudo. The controller requires
Python 3.12+, the maintained Ansible environment, GitHub CLI authentication and curl.

Point the new HTTPS hostname at the destination before running the command, and
open its public HTTP/media/TURN ports as described in the public guide. This first
migration workflow requires different source and destination HTTPS origins; it
does not edit DNS or orchestrate a same-hostname DNS cutover. To preserve existing
passkeys, the new hostname must belong to the source's actual WebAuthn RP ID,
normally the existing hostname or one of its subdomains. The controller reads and
retains that identity rather than trusting an inventory guess.

Keep `scbench_upgrade_packages` and `scbench_reboot` false. Set the destination's
network allowance and port speed from the provider's provisioned values:
`scpub_transfer_allowance_tb: 0` means unlimited, and `scpub_port_mbps` is the
advertised port speed, not a bandwidth measurement. VirtIO often reports an
unknown interface speed.

Service opt-ins belong in the destination inventory:

```yaml
scpub_registration_enabled: true
scpub_turn_enabled: true
scpub_backup_enabled: true
scmon_enabled: true
scmon_external_repository: OWNER/REPOSITORY
```

The controller prepares and activates the relay after HTTPS has obtained its
certificate, then enables the selected local backups and monitoring. Offsite
backup destinations and credentials are separately managed by [BACKUPS.md](BACKUPS.md);
they are not silently copied to another machine.
Prometheus time-series files remain on the retained source disk; the destination
starts a new time-series store. Database-backed operational history is migrated.

## Run one migration

Run the focused checks relevant to the change, push the clean committed `main`
revision directly, and run this command from that same checkout. The complete
local CI gate remains available but is not required before every push. Migration
still requires its exact successful main-push CI and verifies the artifact's
attestation before any remote mutation; it has no force mode.

```sh
ops/ansible/.venv/bin/python build/migrate.py \
  --repository OWNER/REPOSITORY \
  --source-inventory ops/ansible/inventory.local.yml \
  --source-origin https://chat.example.com \
  --source-limit public_vps \
  --inventory ops/ansible/inventory.local.next.yml \
  --origin https://next.chat.example.com \
  --limit public_vps \
  --activate-inventory ops/ansible/inventory.local.yml \
  --update-canary
```

`--activate-inventory` optionally replaces the existing ignored source inventory
with the verified destination configuration. Its original bytes are retained
privately and checked again before replacement; concurrent changes cause refusal.
The destination alias, service opt-ins, exact revision and preserved RP ID remain
in the active inventory. `--update-canary` optionally changes GitHub's existing
`CANARY_ORIGIN` from the source origin to the destination after cutover. It leaves
`CANARY_ROOM` unchanged. Both options are off by default, and their individual
outcomes are retained if either final selector update fails.

The migration preserves all database tables, including accounts, passkey
credentials, rooms, roles, invitations and private operational history. It compares
deterministic complete-row hashes and counts, sequences, migration checksums,
schema definitions, ownership and privileges before starting the destination.
The source and destination must use the same pinned PostgreSQL image and server
version. The bounded archive limit is 512 MiB; oversized or unsupported contents
fail before restoration rather than being skipped.

The managed owner email changes from `owner@<source-domain>` to
`owner@<destination-domain>` in one destination-only transaction. Every other
owner field, including UUID, password hash and authentication version, must remain
identical. Other accounts retain their existing email addresses. Users sign in
again because browser session cookies belong to the original hostname. Live calls
and chat history held only in application memory cannot migrate; they are not
stored in the database archive.

Verification makes bounded anonymous HTTPS GETs and checks the frontend revision;
it does not send chat or create accounts. The configured media canary remains the
separate direct-media and TURN check after switching `CANARY_ORIGIN`.

## Failure and recovery

Each invocation has private controller evidence under `results/migration.*` and
root-private host evidence under
`/srv/simplestchat-public/migrations/<operation-id>/`. These include frozen
inventories, exact invocation variables, original inventory bytes when requested,
artifact selection, database archive hashes, logical snapshots and phase receipts.
Database values, archive contents and credentials must remain private.

A failure before destination startup attempts to confirm its app and proxy are
stopped, then restore the recorded source services. The source helper also attempts
this recovery if archive creation fails. A disconnected controller can leave its
bounded remote freeze unit running; the workload lock then refuses concurrent
recovery until that unit settles. The outcome explicitly reports manual recovery
when automatic recovery cannot finish. Recovery is never allowed after the source's
durable cutover seal: the destination may already have accepted writes even if
the controller lost its connection. At that point repair the destination using
the retained evidence; restarting the old source would create two divergent sites.

Successful retirement stops the old application, database and relay, disables
their managed timers and container restarts, and retains all old volumes and
archives. The VPS itself is not deleted. An incomplete attempt is not replayed
automatically: inspect `outcome.json`, `variables.json`, the phase receipts and
both host journals before any explicit recovery action. Never remove a journal
to bypass an unfinished operation.

If restoration failed before the destination app or proxy was ever created and
source recovery succeeded, the host helper's explicit `abort-target --request
/srv/simplestchat-public/migrations/<operation-id>/request.json` action retains
the failed PostgreSQL directory inside that attempt as `retained-postgres`.
It checks the unfinished destination journal, unchanged configuration and owned
database mount, stops and removes only that PostgreSQL container, then finalizes
the aborted attempt. It never deletes database files or accepts a completed
restore or any existing app/proxy container. After inspecting that result, run a
new migration to take a fresh source archive; do not reuse the failed archive.

CI runs the same controller, preflight and lifecycle tests locally and on GitHub,
plus a disposable PostgreSQL dump/restore test that exercises row preservation,
partitioned storage, schema/ACL drift detection and the owner-email transaction.
