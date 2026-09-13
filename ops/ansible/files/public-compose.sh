#!/usr/bin/env bash
# Fixed project and private configuration; never print resolved config with secrets.
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
unset DOCKER_CONTEXT DOCKER_TLS_VERIFY DOCKER_CERT_PATH DOCKER_CONFIG
[[ $(id -u) == 0 ]]
exec docker --host unix:///var/run/docker.sock compose \
  --project-name simplestchat-public --project-directory /etc/simplestchat-public \
  --env-file /etc/simplestchat-public/app.env \
  -f /etc/simplestchat-public/compose.public.yml "$@"
