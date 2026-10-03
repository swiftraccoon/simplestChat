#!/usr/bin/env bash
# Preserve loopback-only database guards with act's published service ports.
set -euo pipefail
umask 077
[[ "${ACT:-}" == true && "${LOCAL_CI_DISPOSABLE:-}" == 1 ]] || {
  echo 'The PostgreSQL relay is only for build/ci-local.sh runners.' >&2; exit 2;
}
[[ "$(uname -s)" == Linux && ( -f /.dockerenv || -f /run/.containerenv ) ]] || {
  echo 'Refusing the PostgreSQL relay outside a disposable job container.' >&2; exit 2;
}
[[ "${CI_LOCAL_POSTGRES_PORT:-}" =~ ^543[2-5]$ ]] || {
  echo 'The local PostgreSQL service must use its declared CI port.' >&2; exit 2;
}
relay_state="${RUNNER_TEMP:?}/local-ci-postgres"
mkdir "$relay_state"
export CI_LOCAL_POSTGRES_READY="$relay_state/ready"
python3 -u - > "$relay_state/relay.log" 2>&1 <<'PY' &
import asyncio
import ipaddress
import os
import pathlib
import socket

port = int(os.environ["CI_LOCAL_POSTGRES_PORT"])
gateway = socket.gethostbyname("host.docker.internal")
if not ipaddress.ip_address(gateway).is_private:
    raise SystemExit("The CI service gateway must be private")

async def transfer(reader, writer):
    while data := await asyncio.wait_for(reader.read(65536), timeout=120):
        writer.write(data)
        await writer.drain()
    if writer.can_write_eof():
        writer.write_eof()

async def relay(reader, writer):
    upstream = None
    try:
        remote, upstream = await asyncio.wait_for(asyncio.open_connection(gateway, port), timeout=10)
        await asyncio.gather(transfer(reader, upstream), transfer(remote, writer))
    except (OSError, asyncio.TimeoutError):
        pass
    finally:
        writer.close()
        if upstream is not None:
            upstream.close()

async def main():
    server = await asyncio.start_server(relay, "127.0.0.1", port, backlog=32)
    pathlib.Path(os.environ["CI_LOCAL_POSTGRES_READY"]).write_text("ready\n")
    async with server:
        await server.serve_forever()

asyncio.run(main())
PY
relay_pid=$!
for ((attempt = 0; attempt < 100; attempt++)); do
  if [[ -f "$CI_LOCAL_POSTGRES_READY" ]]; then
    printf 'Started the job-local PostgreSQL relay on port %s.\n' "$CI_LOCAL_POSTGRES_PORT"
    exit 0
  fi
  kill -0 "$relay_pid" 2>/dev/null || break
  sleep 0.1
done
cat "$relay_state/relay.log" >&2
echo 'The job-local PostgreSQL relay did not become ready.' >&2
exit 1
