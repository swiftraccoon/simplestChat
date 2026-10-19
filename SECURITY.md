# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub's vulnerability reporting for this
repository (**Security → Report a vulnerability**). Do not open a public issue or
pull request for a security problem, and do not test against a deployment you do
not operate.

Include the affected area (server, web client, deployment automation or CI),
the commit you tested, reproduction steps and the impact you observed. You will
receive an acknowledgement within three days and a status update at least every
two weeks until the report is resolved.

## Scope

- The Rust server (`src/`), the browser client (`web/`), the load generator
  (`load_tests/`) and the release, deployment and host automation (`build/`,
  `ops/`) in this repository.
- Dependencies are in scope when this repository's use of them is the problem;
  report a dependency's own vulnerability to its maintainers and, if it affects
  this project, here as well.

Security requirements and the checks that enforce them are described in
[`docs/security.md`](docs/security.md); scanner configuration and reviewed
exceptions live under [`security/`](security/README.md).

## Supported versions

Only the current `main` branch is supported. Fixes ship through the normal
signed release path and are not backported.

## Disclosure

Reporters are credited in the fix's commit message or release notes if they
wish. Please allow a fix to be deployed before publishing details.
