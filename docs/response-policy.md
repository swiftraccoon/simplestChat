# Passive response policy in the disposable release fixture

The canonical `build/test-release-container.py` harness performs one passive HTTP
policy check after its application and Caddy fixture become ready. The check is
implemented by `build/release_http_policy.py`; it uses only the Python standard
library and the repository's bounded process helper. It does not start another
application, compile an image, or invoke the public chat smoke.

The harness supplies its exact application/Caddy container IDs, ownership token,
private CA path and recorded CA SHA-256. The helper inspects only those IDs through
the fixed local Docker socket. Both containers must be running with this fixture's
ownership label, the expected Compose project, and their exact service labels.
Caddy must publish HTTPS on precisely `127.0.0.1:443`, with no other published
ports. The same checks run again after the responses have been validated.

The client opens the numeric loopback address directly, verifies the certificate
for `localhost`, and trusts only the fingerprint-verified fixture CA. It does not
use a configurable origin, DNS-selected destination, HTTP proxy, global cookie
jar, bearer token or login credentials. Redirects are rejected. There is no retry
loop, endpoint or asset discovery, account operation, chat message, browser action
or active security scan.

## Fixed request and policy coverage

Exactly five ordinary GET requests are made:

| Path | Expected response | Additional assertions |
| --- | --- | --- |
| `/` | `200`, HTML | HTML doctype; body at most 128 KiB. |
| `/api/capabilities` | `200`, JSON | Current public feature contract with no additional/internal fields; `Cache-Control: no-store`; at most 4 KiB. |
| `/api/auth/profile` | `401`, JSON | Exact generic missing-authorization response; at most 4 KiB. |
| `/metrics` | `404`, plain text | Exact public omission response; at most 4 KiB. |
| `/diagnostics/media` | `404`, plain text | Exact public omission response; at most 4 KiB. |

Every response must carry the reviewed Caddy security headers, including HSTS,
MIME sniffing prevention, frame denial, referrer policy, media permissions, opener
isolation and resource isolation. The CSP is parsed into directives and compared
to the complete reviewed policy. Duplicate directives, permissive script-element
overrides and unreviewed additions fail instead of inheriting an apparently safe
`script-src` fallback. The `Server` header must remain absent.

These requests are anonymous and must not set any cookie, including a cookie
with otherwise secure flags. This is an absence check, not a test of authenticated
cookie issuance. Existing Rust authentication tests cover the issued refresh
cookie's host prefix, Secure, HttpOnly, SameSite and path attributes. This helper
does not log in or create a session just to repeat those tests.

The helper rejects duplicate/control-bearing headers, more than 64 headers and
more than 16 KiB of parsed header fields. Python's HTTP parser additionally bounds
individual wire header lines and total header count before this stricter policy
check. Responses must use the expected MIME type and no content compression.
An announced oversized body is rejected before reading it; otherwise the client
reads at most the endpoint budget plus one byte before closing the connection.

Each socket operation has a three-second timeout. A 25-second process deadline
also covers slow TLS/header/body delivery and Docker ownership inspection. Each
Docker inspection has its own five-second deadline and output ceilings. The
parent harness gives the helper 35 seconds and terminates its owned process group
if it does not settle. The helper makes no request until ownership and CA checks
have passed.

## Evidence and existing checks

Only the fixed policy names, five-request count and pass/fail result enter the
harness's sanitized `report.json`, under `cases.httpResponsePolicy`. Response
bodies, header values, cookies, certificate contents and exception details never
enter that report. Normal harness command stdout/stderr remain private. A failed
check prevents the release-container test from passing and therefore prevents
the existing CI publication step from accepting that run.

This check supplements these existing, distinct checks:

- Rust tests verify application middleware, auth guards, successful JSON cache
  policy and refresh-cookie issuance without the production reverse proxy.
- Caddy configuration tests verify the maintained header/private-path policy
  without starting Caddy or contacting a certificate authority.
- The basic container smoke verifies migrations, database-backed API behavior,
  registration and restart on newly created containers.
- The public deployment smoke verifies HTTPS/assets and writes one owned chat
  message on an explicitly selected deployment. It is not invoked here.

The new check establishes response policy for these exact paths in the owned
fixture. It does not establish every route's authorization, account/tenant
isolation, authenticated cache behavior, browser CSP execution, session lifecycle,
media correctness, production network policy or resistance to an active attack.

Run offline parser, transport, ownership and harness-wiring tests with:

```sh
python3 -m unittest discover -s ops/ansible/tests -p test_release_http_policy.py
python3 -m unittest discover -s ops/ansible/tests -p test_release_container_harness.py
```

These tests mock the transport and Docker inspection; they do not contact a
network endpoint. The real check runs only as part of the disposable Linux
release-container harness described in [testing.md](testing.md). Its controller
supports AMD64 and ARM64; the production image under test remains AMD64.
