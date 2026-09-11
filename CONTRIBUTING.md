# Contributing

Changes should make rooms easier to use without weakening privacy, media
ownership, recovery, or delivery guarantees. Small, reviewable changes with
evidence are more useful than broad rewrites or an unsupported capacity claim.

Start with [development setup](docs/development.md), then the
[browser](web/README.md) or [server](src/README.md) module guide. Keep the root
README a quick introduction; put detailed explanations alongside the system
they describe. Local agent instructions such as `CLAUDE.md` stay untracked.

## Before requesting review

After installing the pinned dependencies and native prerequisites, run:

```sh
build/check.sh
```

The same entry point is used by CI. It stops on the first failure and does not
install dependencies, start the application, or create a database. You can run
`--web`, `--rust`, or `--helpers` separately while iterating.

| Group | What it checks | What it does not prove |
| --- | --- | --- |
| Web | Type-aware lint, formatting, source-level tests, TypeScript, production build and asset-size budgets | Real-browser rendering, device permission behavior or media delivery |
| Rust | Formatting, every first-party target/feature with Clippy, warning-free public/private Rustdoc | Database correctness, native runtime behavior or shutdown reliability |
| Helpers | Shell syntax, ShellCheck and helper/benchmark regression tests | Production deployment or an actual performance comparison |

Run the additional checks relevant to the change, using the commands in
[testing](docs/testing.md):

- Rust logic: serialized native/unit tests; include database tests for persisted
  state, migrations, accounts, permissions or room settings.
- Browser behavior: source tests plus the community suite against the rebuilt
  production assets. Include the relevant browsers and manual device journeys.
- Protocol changes: update both languages, validate boundary data, and test the
  serialized forms as well as the receiving behavior.
- Performance-sensitive changes: repeat a representative before/after workload
  under comparable conditions. Correctness tests alone do not establish that
  latency, throughput, memory or media quality stayed within budget.

Record the commands, outcomes and remaining limitations in the change summary.
Do not describe skipped, mocked or unrun paths as verified.

## Type and lint policy

The browser compiler enables strict null/function checking, exact optional
properties, checked indexed access, unused-code checks, explicit override and
return checking, unreachable-code rejection, checked side-effect imports and
library declaration checking. Browser and Node tooling have separate ambient
environments, with a shared strict baseline. A browser module must not acquire
Node globals merely to satisfy the Vite configuration.
Build scripts in `web/scripts/` are checked JavaScript with typed JSDoc; they
use the strict Node-tooling project rather than bypassing it because of their
file extension.

Oxlint's TypeScript 7-compatible native type analysis checks unsafe values,
unhandled promises, async callbacks in synchronous APIs and exhaustive union
switches. Plain `void task()` is not rejection handling. JavaScript test and
browser-harness files receive syntax/correctness lint; their mocked fixtures are
not fully type checked. The source suite also compiles
[contract fixtures](web/tests/type-contracts.ts) with the production compiler to
check result inference and expected type errors. This is separate from the
in-memory transpiler used to run source tests.

- Use installed library types instead of opaque `any` aliases. Accept external
  JSON as `unknown`, validate its complete relevant structure, then narrow it.
  An assertion or a caller-selected generic result type is not validation.
- Use the named HTTP methods in [ui.ts](web/src/ui.ts) and action-selected
  `RoomClient.requestSocial` contracts. New endpoints need their own decoder;
  new social actions need request and response map entries plus runtime decoding.
  Test valid results, invalid shapes and compile-time action/payload mismatches.
- Treat omitted, `null` and a supplied value as separate wire states where the
  operation distinguishes unchanged, clear and set. Do not add `| undefined`
  merely to silence an exact-optional-property diagnostic.
- Keep Rust `unsafe` forbidden in first-party code. Do not hide dependency
  warnings by editing vendored sources; vendor patches require their own
  provenance and verification as described in [vendor notes](vendor/README.md).
- First-party Rust warnings fail the build. Prefer a reasoned `#[expect(...)]`
  for an unavoidable narrow exception; an expectation that stops matching also
  fails. Do not add crate-wide allowances to make unrelated findings disappear.
- Fix the cause of a lint finding. An exceptional boundary may use a narrowly
  scoped, named rule exception with the invariant and reason explained nearby.
  Broad disable comments, unsupported-tool warnings and unchecked casts are not
  an acceptable way to make a gate pass.

The committed rule configuration is authoritative. New rules need a reviewed
baseline and a passing gate; enabling every available rule is not meaningful
when style or restriction rules conflict.

## Async work and media ownership

Every asynchronous operation needs an owner: account intent, room membership,
transport, producer, dialog or preview action. After each await that can outlive
that owner, verify it is still current before applying results. Cancelling a
timer does not cancel a request already in flight.

Specify who stops each captured track and closes each native resource. Late
permission, produce or replacement results must release their resources without
restoring a retired stream. Joining and opening settings never start capture;
preview is private, and saving preferences does not enable an inactive device.
Viewer-local mute, volume and hide settings must not affect another participant.

Detached UI tasks must have an explicit rejection boundary. Keep user-facing
errors useful and scoped to the current account/room. Do not log access tokens,
cookies, ICE credentials, private messages, raw SDP or native errors containing
such data. A completed ICE restart only confirms that credentials were applied;
it does not prove connectivity or decoded media.

Test late success and late failure, cancellation, duplicate events, partial
success and retry. A happy-path assertion does not cover resource ownership.

## Documentation standard

Document the contract a maintainer or operator needs, not a narration of each
line. For non-obvious public functions and lifecycle boundaries, explain:

- Purpose, valid inputs and the returned result or observable side effects.
- Ownership, concurrency, cancellation and which state must remain consistent.
- Expected failures, recovery behavior and whether retry is safe.
- Security or privacy assumptions and configuration dependencies.
- A working example when the calling convention or wire representation is easy
  to misuse.

Keep Rustdoc and TypeScript doc comments close to those contracts. Shared guides
should contain runnable commands, prerequisites, actual defaults and limits.
The Rust gate denies documentation warnings, including broken links and invalid
HTML in public and private items; it does not check whether every public item
is documented or whether its explanation is complete. Review contract quality
alongside the code; a clean documentation build is not a coverage claim.
Update configuration, migrations, protocol examples and user-facing guidance
with the implementation. A new shared file under `docs/` needs an explicit
allowlist entry in `.gitignore`; do not publish private plans accidentally.

## User experience and release evidence

Controls need an accessible name and a keyboard-operable native element.
Dialogs must manage focus, dismissal and small-screen overflow consistently.
Differentiate local preview, publishing, muted capture, blocked playback,
reconnecting and failed media; a connected WebSocket alone is not evidence of
working audio/video. Include screen-reader, keyboard, zoom, mobile and denied-
permission checks when affected. Automated accessibility checks are not a claim
of full conformance.

Before deployment, verify the [release checklist](docs/testing.md#manual-release-checklist)
and [deployment limitations](docs/deployment.md#limitations-and-operational-caveats).
Public-platform readiness also needs current capacity measurements, recovery
drills and operational monitoring. Keep those limitations visible until the
corresponding behavior is implemented and verified.

Use focused commits with an imperative subject and explain the reason for
non-obvious changes in the body. Keep mechanical formatting separate from
behavior changes where practical. Do not commit secrets, captured user data,
generated build outputs or local agent instructions.
