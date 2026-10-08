# Authorization policy checks

The authorization suite checks the server’s decisions with fixed, local fixtures.
It invokes the real Axum router and WebSocket dispatcher in process, and uses a
fresh PostgreSQL cluster for persisted identity and room authority. It does not
scan a running deployment, generate network traffic, search for vulnerabilities,
or change production admission thresholds.

## Run the checks

Complete the pinned Rust and OpenSSL setup in [development](development.md).
The focused command creates and stops only its own local PostgreSQL cluster:

```sh
build/with-test-postgres.sh \
  cargo test --locked --all-features --lib authorization_ -- \
  --include-ignored --test-threads=1
```

The six focused database tests require both `TEST_DATABASE_URL` and
`DISPOSABLE_TEST_DATABASE=1`; the maintained helper sets them. These tests apply
the tracked migrations before creating their own random account and room IDs.
They delete their own rows in dependency order. A failure leaves the helper’s
private database directory and logs available for diagnosis; it never selects a
shared database or deletes objects by a name prefix.

For the full existing credential, reconnect, and room-control coverage, use the
complete serial Rust/database command in [testing](testing.md). The focused
`authorization_` filter alone does not execute every existing test referenced in
the policy manifest. A source-only run without `--include-ignored` deliberately
skips database checks and is not a complete authorization result.

## What the matrices establish

| Boundary | Executed checks |
| --- | --- |
| Role hierarchy | All 36 actor/target moderation pairs and all 216 actor/target/granted-role combinations, using explicit expected allowlists independent of the production numeric comparisons. Settings, lobby admission, chat and broadcasting are checked for all six roles. |
| Moderation dispatch | All 36 actor/target pairs reach the real camera-close and role-control handlers. Authorized requests are successful no-ops; denied requests leave the target’s role unchanged. All six roles also exercise ban/member/report/history reads and settings/topic controls at their distinct thresholds. No media is published, and the administration rate bucket is neither replaced nor disabled. |
| Current HTTP sessions | All 31 session-authenticated method/handler pairs reject missing credentials, another account’s session ID, an absent session ID, a stale authentication version, and an expired database session. A valid own-session profile read succeeds and returns that account’s ID. |
| Room authority | Real invitation list/create/revoke handlers check ordinary user, member, moderator, administrator and owner roles. An administrator may invite only to lower roles. Ownership in one room and membership in another are resolved independently. |
| Tenant and account scope | A valid invitation ID from a different room cannot be revoked through the caller’s own room URL. One account cannot revoke another’s registration invitation. Only the room owner may update its identity. Denied mutations are followed by persisted-state checks. |
| Socket membership | All 49 room-bound commands reject a replaced sender and then a removed participant through the real dispatcher, despite the socket’s retained room ID. No success reply is emitted to the unauthorized sender. |
| Lobby isolation | Every command declared inadmissible while waiting is decoded and dispatched against lobby state; it fails before room work or a response. Join, leave, reconnect and lobby chat have explicit distinct entries. Authentication renewal is a socket operation intercepted before room dispatch. |
| Public and credential-establishment HTTP | Fixed cases exercise public health, readiness, capabilities, directory and profile reads; anonymous bounded telemetry; closed registration; credential/cookie/challenge requirements; and protected metrics. Anonymous passkey discovery must not disclose account credential IDs. Public profiles must not contain email, password/recovery hashes or authentication versions. |

The role matrix is a policy oracle, not an alternate implementation of the role
ordering. The handler tests independently demonstrate where that policy reaches
HTTP and signaling behavior. Passing the role helper matrix alone would not
establish that a handler invoked it.

## Operation inventory

[`security/authorization/operations.json`](../security/authorization/operations.json)
contains all 49 explicit Axum method/path/handler combinations and all 53
`ClientMessage` variants. Each entry names its boundary and executable checks;
message entries also contain a reviewed minimal decoded fixture. These inputs
are synthetic application data, not captured credentials or exploit payloads.

The inventory test parses the actual Rust router syntax, follows its named
routers, `nest` prefixes, merged routers and chained methods, and compares the
result with the manifest. It requires the actual handler path, not merely a URL.
An unfamiliar router-construction form fails until the checker is deliberately
extended. Axum’s implicit `HEAD` behavior for `GET`, static-file fallback and
framework-generated method errors are outside this explicit-operation inventory.

For signaling, the test compares the complete enum variant set, decodes every
fixture with the production Serde type, checks its serialized operation name,
and uses an exhaustive Rust match to bind its boundary. Adding a variant breaks
that match until it receives a policy decision. The inventory also verifies that
referenced checks are actual Rust test functions; removing or renaming one cannot
silently leave a stale coverage claim.

A new route or command therefore requires an explicit manifest entry, a valid
fixture where applicable, and a test of its relevant authority. Existing checks
may be shared when they exercise the same real boundary; citing a helper’s name
is not a substitute for testing an operation-specific rule. Review additions for
both allowed and denied outcomes and for the absence of side effects after denial.

## Scope and interpretation

These are deterministic regression checks, not proof of the entire authorization
system. Baseline route/session and socket-membership coverage is exhaustive over
the current explicit operation set; every combination of every handler’s domain
parameters is not. Media ownership, punitive-state persistence, password and
passkey verification, challenge expiry, invitation redemption, logout/refresh,
concurrent credential removal, reconnect binding and credential continuity have
additional focused tests elsewhere in the Rust suite.

The manifest references the existing real ticket/upgrade, same-account renewal,
reconnect identity and room-isolation checks for their distinct boundaries. A
missing HTTP bearer is not the authorization model for public password login,
refresh cookies, guest upgrades or an already authenticated socket. Those cases
remain explicit instead of being counted as generic role denials.

The in-process HTTP fixtures give independent cases distinct synthetic
source addresses so rate-limit state does not mask an authorization decision.
They do not test rate-limiter throughput or permit a production limit override.
The native worker starts only to support the ordinary room fixtures; these tests
do not publish RTP or calibrate capacity. Real PostgreSQL behavior is exercised,
but this unit does not add a contention benchmark or a distributed deployment
model.
