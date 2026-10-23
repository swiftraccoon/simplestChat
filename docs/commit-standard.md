# Commit standard

Adopted 2026-10-22 at the user's request: **Conventional Commits 1.0.0**.
This standard applies prospectively to new commits and future work. Adoption
does not establish that existing messages comply; a retrospective audit is
separate work.

The [versioned specification](https://www.conventionalcommits.org/en/v1.0.0/)
is normative. Its
[upstream repository](https://github.com/conventional-commits/conventionalcommits.org)
and [versioned source](https://github.com/conventional-commits/conventionalcommits.org/blob/master/content/v1.0.0/index.md)
provide reference material. Upstream `content/next` is development work; future
changes do not silently replace this adoption. The repository choices below
supplement the specification and the [coding standard](coding-standard.md).

## Specification requirements

The subject has this structure; bracketed parts are optional placeholders:

```text
<type>[optional scope][!]: <description>
```

Use `feat` for a new application or library feature and `fix` for a bug fix. The
specification permits other noun types. An optional scope names a codebase area
in parentheses. The colon and following space are required. A body, when used,
starts after a blank line; footers follow after a blank line.

A breaking change uses `!` immediately before the colon, a
`BREAKING CHANGE: <explanation>` footer, or both. The specification also accepts
`BREAKING-CHANGE` as the footer token. Breaking changes can occur with any type.
Other footer tokens use hyphens rather than spaces, followed by `: ` or ` #`.
These requirements come from the
[1.0.0 specification](https://www.conventionalcommits.org/en/v1.0.0/#specification).

## Repository choices

Use lowercase types and scopes and a concise, imperative description of the
result. Scope is optional when a coherent change spans areas. The existing hook
and CI accept these types:

| Type | Use |
| --- | --- |
| `feat` | New behavior or capability |
| `fix` | Bug correction |
| `docs` | Documentation |
| `style` | Formatting or style without a behavior change |
| `refactor` | Behavior-preserving restructuring |
| `perf` | Performance work |
| `test` | Tests and fixtures |
| `build` | Build, dependency or packaging changes |
| `ci` | CI automation |
| `chore` | Other maintenance |
| `revert` | Revert a commit; identify its hash in the body or footer |
| `ops` | Operational or deployment behavior |

This closed list is a repository choice, not an upstream restriction. Optional
scopes follow the existing grammar `[a-z0-9][a-z0-9._/-]*`. Choose a meaningful
area, rather than forcing every change into one bucket:

| Scope | Area |
| --- | --- |
| `server` | Server behavior, accounts, rooms and persistence |
| `web` | Browser application and its tests |
| `protocol` | Shared wire contracts and generated client types |
| `media` | Capture, transport, subscriptions and playback |
| `tooling` | xtask, build tools and local validation |
| `ops` | Deployment, release and operator tooling |
| `security` | Security policy, verification and exact review records |

Every breaking change requires a `BREAKING CHANGE:` footer explaining its
compatibility impact and the migration, including subjects that use `!`.
Requiring that footer even with `!` is an additional repository rule. It applies
during initial development as well as after a release. Document the contract
change and update its producers, consumers, tests and guidance together.

## Coherent completed checkpoints

Plan commit boundaries before staging. Make commits at meaningful completed
checkpoints, sized by coherent change rather than implementation step. One
commit carries one theme: a feature, the redesign of one area, a bug with its
fix, or one maintenance concern, together with the tests, documentation,
generated files, lockfiles, security review rebindings, allowlist updates and
measurement evidence that the change requires.

Do not split a theme into a trail of small commits: a type, a test move, a review
rebinding and an allowlist line are parts of the same change when it needs them.
Fold thematic follow-ups into the unpublished commit that requires them; squash
step-by-step work before pushing. Do not bundle independent themes either. A
review's whole implementation can require several commits, one per theme. As a
yardstick, a day's work on one roadmap slice is a handful of commits, not dozens.
Keep mechanical formatting separate from behavior changes where practical.

Each commit must stand on its own: its subject names the theme, and its body
explains non-obvious decisions and relevant validation. Review the staged diff
and its file summary before committing. Record the checks actually run and
their limits; distinguish source checks, inert fixtures, real services, browser
behavior and measured workloads. Preserve failed trials and their explanations
with the corresponding evidence, while keeping private captures out of Git.
Never commit secrets, captured user data, generated build outputs or local agent
instructions.

Once published, correct a commit with a new focused commit; do not rewrite
published history. The folding rule applies to unpublished work. Existing main
branch deletion and force-push protections remain in place.

## Validation and publication

Enable the shared local subject check in each checkout with:

```sh
git config --local core.hooksPath .githooks
```

The [.githooks/commit-msg](../.githooks/commit-msg) hook checks the first line.
CI's `xtask ci commit-subject` checks the selected branch tip, using the actual
PR head instead of GitHub's generated merge message. Both validate subject
syntax only. They do not establish that a description is imperative, that a
scope fits, that a commit is coherent, or that its body, breaking footer and
verification claims satisfy this standard. Those require human review. This
adoption changes documentation, not hook or CI enforcement.

Commit conventions do not automatically change package versions, create tags
or releases, push commits, or deploy. Perform those actions when they are part
of the authorized task, following the existing
[publishing and verification rules](../CONTRIBUTING.md#before-publishing).
Existing authorization for commits, pushes and deployments continues to apply;
this standard adds no approval step or requirement to obtain it again.

## Examples

These illustrate message shapes; they do not claim the changes have been made:

```text
feat(web): preserve the draft during room recovery
fix(server): reject expired room invitations
refactor(media): share consumer retirement ownership
build(tooling): pin the native build inputs
docs: adopt Conventional Commits 1.0.0
ops: retain the prior release before replacement
```

```text
feat(protocol)!: require room identifiers in control acknowledgements

Bind each acknowledgement to the room whose pending action it completes.

BREAKING CHANGE: control acknowledgements must carry roomId. Update clients to
validate it and deploy matching server and client contracts together.
```
