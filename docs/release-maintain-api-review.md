# Prepared-host maintenance: affected API review

This prospective review covers `release maintain`, borrowed normal-launcher
transport, two actual workload-lock acquisitions, the fixed Caddy publication
transition, and expected-old Ops installation. It reuses the established
[stage](release-stage-api-review.md),
[deployment](release-deploy-api-review.md),
[native launcher](maintain-native-prerequisite-api-review.md) and
[controller](maintain-controller-api-review.md) decisions. The project
[coding standard](coding-standard.md) and [API inventory](rust-api-checklist.md)
apply; this is not a retrospective crate or dependency certification.

## Transaction and authority

The new public synchronous entry is `release::maintain::run`. It accepts one
exact revision, explicit `0 < M < N <= 1800`, an optional protected candidate
environment and an explicit browser-proxy upgrade. The common quiet option
retains the frozen reference's behavior: maintenance accepts its bounded value
without waiting for quiet rooms. Help and license routes precede host effects.
Invalid syntax returns 2; policy or operation failure returns 1. A nonzero final
exit invalidates any earlier complete or partial report.

Maintenance verifies staging, current identities and running configuration,
then admits applied migration checksums as a subset of packaged migrations,
including holes. It preserves all sixteen identity settings with absent/empty
equivalence. Reviewed settings and proxy changes can authorize same-image
maintenance. Effective Compose comparison remains complete around those grants.
A proxy upgrade admits the exact committed Caddyfile, validates it with the
running pinned proxy and publishes it 0644. Other private records remain 0600.
Its old hardlinked generation admits only the exact link-count decrement caused
by the known replacement; this does not widen other private-file policies.

The verified live dump, list completion, flushed publication and receipt precede
app interruption. The app stops before the proxy. Selection, browser revision,
optional public Caddy generation and candidate consumption precede the finalized
`maintenance_selected` journal. Every asset-store borrower finishes before the
first actual unlock. An owner fence after unlock prevents launcher continuation
after a failed release. The normal launcher borrows that same parent owner and
does not register another outer signal owner or restart its origin.

After the launcher, a freshly opened descriptor must acquire an actual flock on
the original lock inode. Reentry preserves the attempt, original source/tool/
archive bindings, owned atomic records and exact selected journal. A newly
finalized benchmark journal can replace the first hold's guard, which was
explicitly closed while that first lock was still held. Unfinished benchmark or
release state remains refused. Final verification requires readiness, selected
app image/configuration, unchanged database CID and exact final migration ledger.
Launcher failure keeps the selected state; it starts no application rollback.

## Clock, observations and ownership

One original owner includes parsing, runtime, signal registration, all known
finalizers, output and umask restoration. Source-derived first-stop debt is 1411 s:
app stop 55, proxy stop 40, native work/transport 925 plus its explicit local tail 60,
and final readiness/queries 331. The older 1351 s floor excluded this local tail.
App stop and proxy/selection have separate ceilings within the same original
body. The borrowed launcher end is at most its entry+1050 and the parent body
minus 331; its body ends 60 seconds earlier. The transaction's local reserve is
never borrowed. Slow preparation can refuse before interruption. Blocking kernel
I/O, reap and close still have no hard wall-clock guarantee.

The passive `maintenanceEvidence` record has four nullable original-owner elapsed
nanosecond observations: backup publication, first stop dispatch, first unlock
and successful guarded second acquisition. Dispatch means entering the bounded
executor, not that Docker accepted or completed the stop. Actual process results,
daemon events, original files and cleanup remain separate proof obligations.
Native launcher facts survive a negative result; partial false/null observations
never become successful finalization evidence.

`C-VALIDATE`, `C-CUSTOM-TYPE`, `C-NEWTYPE`, `C-STRUCT-PRIVATE` and `C-SERDE` apply
to closed roles, requests, original-file custody, private lock capabilities and
passive records. Live owners do not serialize. `C-CTOR`, `C-FAILURE` and
`C-GOOD-ERR` apply through fallible setup, typed first causes and bounded fixed
diagnostics; candidate values, secrets and raw native stderr are not public
failure messages. Valid setting names remain the documented change report.
`C-DTOR-FAIL`, `C-DTOR-BLOCK` and `C-RW-VALUE` apply through explicit peer finishes,
inherited input ownership, bounded bulk outputs and joined process/stream peers.
`C-COMMON-TRAITS` is assessed by actual role: passive all-null evidence has a
valid `Default`; unique lock/signal/process owners are not copied or defaulted.
The new synchronous entry documents its Tokio-context panic prerequisite and
an executable help example (`C-EXAMPLE`, `C-QUESTION-MARK`).

## Distribution and validation limits

The explicit `distribution::install::upgrade` API accepts a closed schema 2 with
an expected old binary digest and length. Schema 1 first-install/exact-repeat
behavior is preserved. Under the actual workload lock, only the expected old
inode may be exchanged for the fully checked and flushed candidate. Both names,
hashes and original owners rejoin before the known old name is retired. A late
failure keeps the published candidate; uncertain pending state is refused.
Expected-old pins describe custody, not signature authentication.

Authenticated export must compose from the opaque current acquisition, retain
it through exact three-member transfer and installer-request publication, then
rejoin original files and current verification before actual exit 0. A serialized
observation or an `authenticated` boolean cannot construct this authority.
SSH delivery and the installed caller remain separate until whole-operation
runtime evidence supports their cutover. All existing Python files stay frozen.

Local transaction tests execute real files, backup publication and flock
handoffs with finite synthetic daemon/launcher responses. They cover migration
holes with an exact final ledger, reviewed settings/candidate consumption,
post-selection refusal, lock contention, changed selected files/database,
ledger mismatch and backup/identity failures before interruption. These tests
do not establish live Docker, SQL migration, TLS or native pipe behavior.
The separate prepared-host signed fixture must establish those applicable live
conditions and actual invocation/job/workflow completion. The earlier four-case
prerequisite proof and its original timing/containment criteria remain unchanged.
